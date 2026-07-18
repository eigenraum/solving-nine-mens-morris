/**
 * Hand-rolled forward pass for the value network, reading the raw weight
 * blob written by `ml/nmm/export.py`. Mirrors ml/nmm/model.py::NmmNet
 * exactly: input(52->H)+ReLU, then n_blocks residual blocks
 * (fc1(H->H)+ReLU, fc2(H->H), +skip, ReLU), then wdl_head(H->3) and
 * depth_head(H->1)+sigmoid. No onnxruntime-web dependency -- this is
 * ~100 lines of matmuls, verified against PyTorch via golden.json (see
 * verify_golden.ts) to <=1e-4 max abs logit difference.
 */

export interface ModelManifest {
  feature_dim: number;
  hidden: number;
  n_blocks: number;
  layers: { name: string; in: number; out: number }[];
  depth_scale: number;
}

interface Layer {
  name: string;
  inFeatures: number;
  outFeatures: number;
  weight: Float32Array; // row-major [out, in]
  bias: Float32Array; // [out]
}

export class NmmNetJS {
  readonly hidden: number;
  readonly nBlocks: number;
  private readonly input: Layer;
  private readonly blocks: { fc1: Layer; fc2: Layer }[];
  private readonly wdlHead: Layer;
  private readonly depthHead: Layer;

  constructor(manifest: ModelManifest, blob: ArrayBuffer) {
    this.hidden = manifest.hidden;
    this.nBlocks = manifest.n_blocks;

    let offset = 0;
    const readLayer = (meta: { name: string; in: number; out: number }): Layer => {
      const wCount = meta.out * meta.in;
      const weight = new Float32Array(blob, offset, wCount);
      offset += wCount * 4;
      const bias = new Float32Array(blob, offset, meta.out);
      offset += meta.out * 4;
      return { name: meta.name, inFeatures: meta.in, outFeatures: meta.out, weight, bias };
    };

    let li = 0;
    this.input = readLayer(manifest.layers[li++]);
    this.blocks = [];
    for (let i = 0; i < this.nBlocks; i++) {
      const fc1 = readLayer(manifest.layers[li++]);
      const fc2 = readLayer(manifest.layers[li++]);
      this.blocks.push({ fc1, fc2 });
    }
    this.wdlHead = readLayer(manifest.layers[li++]);
    this.depthHead = readLayer(manifest.layers[li++]);
  }

  private static linear(x: Float32Array, layer: Layer): Float32Array {
    const { inFeatures, outFeatures, weight, bias } = layer;
    const out = new Float32Array(outFeatures);
    for (let o = 0; o < outFeatures; o++) {
      let acc = bias[o];
      const rowOff = o * inFeatures;
      for (let i = 0; i < inFeatures; i++) {
        acc += weight[rowOff + i] * x[i];
      }
      out[o] = acc;
    }
    return out;
  }

  private static relu(x: Float32Array): Float32Array {
    const out = new Float32Array(x.length);
    for (let i = 0; i < x.length; i++) out[i] = x[i] > 0 ? x[i] : 0;
    return out;
  }

  /** features[52] -> {wdlLogits[3], depth} (depth in [0,1], * depth_scale to
   * get plies, matching ml/nmm/model.py's sigmoid(depth_head) output). */
  forward(features: Float32Array): { wdlLogits: Float32Array; depth: number } {
    let z = NmmNetJS.relu(NmmNetJS.linear(features, this.input));
    for (const block of this.blocks) {
      const h = NmmNetJS.relu(NmmNetJS.linear(z, block.fc1));
      const branch = NmmNetJS.linear(h, block.fc2);
      const summed = new Float32Array(z.length);
      for (let i = 0; i < z.length; i++) summed[i] = z[i] + branch[i];
      z = NmmNetJS.relu(summed);
    }
    const wdlLogits = NmmNetJS.linear(z, this.wdlHead);
    const depthRaw = NmmNetJS.linear(z, this.depthHead)[0];
    const depth = 1 / (1 + Math.exp(-depthRaw)); // sigmoid
    return { wdlLogits, depth };
  }
}

export function softmax3(logits: Float32Array): [number, number, number] {
  const m = Math.max(logits[0], logits[1], logits[2]);
  const e0 = Math.exp(logits[0] - m);
  const e1 = Math.exp(logits[1] - m);
  const e2 = Math.exp(logits[2] - m);
  const sum = e0 + e1 + e2;
  return [e0 / sum, e1 / sum, e2 / sum];
}

export async function loadModel(baseUrl: string): Promise<NmmNetJS> {
  const manifest: ModelManifest = await (await fetch(`${baseUrl}/model.json`)).json();
  const blob = await (await fetch(`${baseUrl}/model.bin`)).arrayBuffer();
  return new NmmNetJS(manifest, blob);
}
