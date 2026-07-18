/**
 * Verifies NmmNetJS against golden.json (exact PyTorch outputs for a fixed
 * set of inputs), exported alongside model.bin/model.json by
 * `ml/nmm/export.py`. Gate: max abs logit/depth difference <= 1e-4
 * (implementation-nn.md N8).
 *
 * Usage: node dist/verify_golden.js <export-dir>
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { NmmNetJS, type ModelManifest } from "./nn.js";

interface Golden {
  inputs: number[][];
  wdl_logits: number[][];
  depth: number[];
}

function main() {
  const dir = process.argv[2] ?? "export";
  const manifest: ModelManifest = JSON.parse(readFileSync(join(dir, "model.json"), "utf-8"));
  const blobBuf = readFileSync(join(dir, "model.bin"));
  const blob = blobBuf.buffer.slice(blobBuf.byteOffset, blobBuf.byteOffset + blobBuf.byteLength);
  const golden: Golden = JSON.parse(readFileSync(join(dir, "golden.json"), "utf-8"));

  const net = new NmmNetJS(manifest, blob);

  let maxLogitDiff = 0;
  let maxDepthDiff = 0;
  for (let i = 0; i < golden.inputs.length; i++) {
    const feats = new Float32Array(golden.inputs[i]);
    const { wdlLogits, depth } = net.forward(feats);
    for (let k = 0; k < 3; k++) {
      const diff = Math.abs(wdlLogits[k] - golden.wdl_logits[i][k]);
      if (diff > maxLogitDiff) maxLogitDiff = diff;
    }
    const dDiff = Math.abs(depth - golden.depth[i]);
    if (dDiff > maxDepthDiff) maxDepthDiff = dDiff;
  }

  console.log(`checked ${golden.inputs.length} golden vectors`);
  console.log(`max abs logit diff: ${maxLogitDiff}`);
  console.log(`max abs depth diff: ${maxDepthDiff}`);

  const THRESHOLD = 1e-4;
  if (maxLogitDiff > THRESHOLD || maxDepthDiff > THRESHOLD) {
    console.error(`FAILED: exceeds ${THRESHOLD} threshold`);
    process.exit(1);
  }
  console.log("golden vector check passed");
}

main();
