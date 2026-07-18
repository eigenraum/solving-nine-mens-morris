"""Export a trained checkpoint (and parity-test fixtures) for the web runtime.

implementation-nn.md N8: writes (a) model.onnx for tooling/interop, (b) a raw
little-endian weight blob (model.bin) + model.json (shapes/feature spec) for the
hand-rolled TS forward pass, (c) golden.json (input vectors + exact fp32
outputs) to verify web/src/nn.ts against PyTorch, and (d) fixtures used by the
web/ rules- and geometry-parity tests (independent of any trained model).
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
import torch

from . import movegen, ranking, symmetry
from .board import ADJ, MILLS, POINT_MILLS
from .db import Database
from .features import FEATURE_DIM, featurize
from .model import NmmNet

# ---------------------------------------------------------------------------
# Model export: raw weight blob + manifest + golden vectors
# ---------------------------------------------------------------------------


def _layer_blob(linear: torch.nn.Linear) -> bytes:
    """Flatten a Linear layer as (out_features x in_features) weight rows
    followed by the bias vector, both float32 little-endian -- the exact
    layout web/src/nn.ts expects (row-major W, then b)."""
    w = linear.weight.detach().cpu().numpy().astype("<f4")
    b = linear.bias.detach().cpu().numpy().astype("<f4")
    return w.tobytes() + b.tobytes()


def export_model(checkpoint_path: str, out_dir: str, n_golden: int = 64, seed: int = 0):
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)

    ckpt = torch.load(checkpoint_path, map_location="cpu", weights_only=True)
    model = NmmNet(hidden=ckpt["hidden"], n_blocks=ckpt["n_blocks"])
    model.load_state_dict(ckpt["state_dict"])
    model.eval()

    # --- ONNX ---
    dummy = torch.zeros(1, FEATURE_DIM, dtype=torch.float32)
    torch.onnx.export(
        model,
        (dummy,),
        str(out / "model.onnx"),
        input_names=["features"],
        output_names=["wdl_logits", "depth"],
        dynamic_axes={"features": {0: "batch"}, "wdl_logits": {0: "batch"}, "depth": {0: "batch"}},
        opset_version=17,
    )

    # --- raw weight blob ---
    # Layout: input(52->H), then n_blocks * [fc1(H->H), fc2(H->H)], then
    # wdl_head(H->3), depth_head(H->1). Each layer: W (out*in floats) then b
    # (out floats), all little-endian float32. web/src/nn.ts must reproduce
    # this order exactly -- model.json's `layers` list states shapes so the
    # TS loader doesn't have to hardcode them.
    blob = bytearray()
    layers_meta = []

    def add_layer(name: str, linear: torch.nn.Linear):
        blob.extend(_layer_blob(linear))
        layers_meta.append(
            {"name": name, "in": linear.in_features, "out": linear.out_features}
        )

    add_layer("input", model.input)
    for i, block in enumerate(model.blocks):
        add_layer(f"block{i}.fc1", block.fc1)
        add_layer(f"block{i}.fc2", block.fc2)
    add_layer("wdl_head", model.wdl_head)
    add_layer("depth_head", model.depth_head)

    (out / "model.bin").write_bytes(bytes(blob))

    manifest = {
        "feature_dim": FEATURE_DIM,
        "hidden": model.hidden,
        "n_blocks": model.n_blocks,
        "layers": layers_meta,
        "activation": "relu",
        "residual_blocks": True,
        "wdl_softmax": True,
        "depth_sigmoid": True,
        "depth_scale": 255.0,
        "feature_spec": {
            "0-23": "mover occupancy bits",
            "24-47": "opponent occupancy bits",
            "48": "mover_count/9",
            "49": "opponent_count/9",
            "50": "mover has exactly 3 stones",
            "51": "opponent has exactly 3 stones",
        },
    }
    (out / "model.json").write_text(json.dumps(manifest, indent=2))

    # --- golden vectors ---
    rng = np.random.default_rng(seed)
    movers = rng.integers(0, 1 << 24, size=n_golden, dtype=np.int64).astype(np.uint32)
    opps = np.zeros(n_golden, dtype=np.uint32)
    for i in range(n_golden):
        # ensure disjoint, plausible stone counts (3..9 each)
        w = int(rng.integers(3, 10))
        b = int(rng.integers(3, 10))
        pts = rng.permutation(24)
        mv = 0
        for p in pts[:w]:
            mv |= 1 << int(p)
        op = 0
        for p in pts[w : w + b]:
            op |= 1 << int(p)
        movers[i] = mv
        opps[i] = op

    feats = featurize(movers, opps)
    with torch.no_grad():
        logits, depth = model(torch.from_numpy(feats))
    golden = {
        "inputs": feats.tolist(),
        "wdl_logits": logits.numpy().tolist(),
        "depth": depth.numpy().tolist(),
    }
    (out / "golden.json").write_text(json.dumps(golden))

    print(f"exported model to {out}/ (model.onnx, model.bin, model.json, golden.json)")


# ---------------------------------------------------------------------------
# Geometry fixture (board.ts / symmetry.ts parity -- no model needed)
# ---------------------------------------------------------------------------


def export_geometry(out_dir: str):
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    data = {
        "adj": [int(x) for x in ADJ.tolist()],
        "mills": [int(x) for x in MILLS.tolist()],
        "point_mills": [[int(a), int(b)] for a, b in POINT_MILLS],
        "perms": symmetry.PERMS.tolist(),
    }
    (out / "geometry.json").write_text(json.dumps(data))
    print(f"exported geometry fixture to {out}/geometry.json")


# ---------------------------------------------------------------------------
# Rules trace fixture (rules.ts parity -- no model needed)
# ---------------------------------------------------------------------------


def export_rules_trace(db_dir: str, out_dir: str, n_traces: int = 10_000, seed: int = 0):
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    db = Database(db_dir)
    rng = np.random.default_rng(seed)
    pairs = db.available_pairs
    sizes = np.array([db.size(w, b) for (w, b) in pairs], dtype=np.float64)
    probs = sizes / sizes.sum()

    traces = []
    draws_per_pair = rng.multinomial(int(n_traces * 1.2) + len(pairs), probs)
    for (w, b), n_draw in zip(pairs, draws_per_pair):
        if n_draw == 0 or len(traces) >= n_traces:
            continue
        size = db.size(w, b)
        idx = rng.integers(0, size, size=n_draw)
        mover, opp = ranking.unindex_batch(w, b, idx)
        canon = symmetry.is_canonical_batch(mover, opp)
        mover, opp = mover[canon], opp[canon]
        for i in range(len(mover)):
            m, o = int(mover[i]), int(opp[i])
            moves = movegen.moves_movement(m, o)
            traces.append(
                {
                    "mover": m,
                    "opp": o,
                    "moves": [
                        {
                            "src": mv.src,
                            "dst": mv.dst,
                            "captured": mv.captured,
                            "successorMover": mv.successor_mover,
                            "successorOpp": mv.successor_opp,
                        }
                        for mv in moves
                    ],
                }
            )
            if len(traces) >= n_traces:
                break
        if len(traces) >= n_traces:
            break

    (out / "rules_trace.json").write_text(json.dumps(traces))
    print(f"exported {len(traces)} rules traces to {out}/rules_trace.json")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    sub = p.add_subparsers(dest="cmd", required=True)

    pm = sub.add_parser("model")
    pm.add_argument("--checkpoint", required=True)
    pm.add_argument("--out", required=True)
    pm.add_argument("--n-golden", type=int, default=64)

    pg = sub.add_parser("geometry")
    pg.add_argument("--out", required=True)

    pr = sub.add_parser("rules-trace")
    pr.add_argument("--db", required=True)
    pr.add_argument("--out", required=True)
    pr.add_argument("--n-traces", type=int, default=10_000)

    args = p.parse_args()
    if args.cmd == "model":
        export_model(args.checkpoint, args.out, args.n_golden)
    elif args.cmd == "geometry":
        export_geometry(args.out)
    elif args.cmd == "rules-trace":
        export_rules_trace(args.db, args.out, args.n_traces)


if __name__ == "__main__":
    main()
