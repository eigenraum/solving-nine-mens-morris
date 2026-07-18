# ML: Neural Compression of the Solved Database

This directory trains a small PyTorch value network as a *lossy-compressed* stand-in
for the ~18 GB solved mid/endgame database, so a browser page can play (near-)perfectly
with no server and no multi-gigabyte download. It is a compression problem, not a
generalization problem — see [`../design-nn.md`](../design-nn.md) for the design,
[`../implementation-nn.md`](../implementation-nn.md) for the milestone build plan, and
[`RESULTS-nn.md`](RESULTS-nn.md) for what has actually been run so far and with what
numbers.

The network predicts, for the side to move, win/draw/loss probabilities plus a depth
estimate. There is no policy head: moves are chosen by evaluating every legal
successor (1-ply lookup), exactly how a consumer would use the real database. The
model covers the **movement/endgame phase** (both sides fully placed, 3–9 stones
each), same as the database; the browser plays the 18-ply placement phase with a
heuristic (`../web/src/placement.ts`).

## Prerequisites

1. **A solved database.** Build the Rust solver and produce one (see
   [`../getting-started.md`](../getting-started.md)):

   ```sh
   cargo build --release
   ./target/release/ninemm solve --dir db --max-total 10   # partial, ~330 MB, minutes — fine for development
   # or, for the real thing (hours of compute, ~18 GB disk):
   ./target/release/ninemm solve --dir db
   ./target/release/ninemm verify --dir db
   ```

2. **Python ≥ 3.11** with [`uv`](https://docs.astral.sh/uv/) (or plain pip):

   ```sh
   cd ml
   uv sync --extra dev          # or: pip install -e '.[dev]'
   uv run pytest                # fast unit gates
   uv run pytest -m slow        # full-scan / large-sample gates (minutes)
   ```

The Python package (`nmm/`) is a deliberately independent re-implementation of the
database reader, symmetry group, indexing, and move generator — it reads the `db/`
directory directly and shares no code with the Rust solver. That independence is a
feature: its consistency gates once caught a real solver bug (see `RESULTS-nn.md`,
"A bug found along the way").

## Training

All commands run from `ml/`. The trainer streams uniformly-sampled states from the
database — there is no dataset-preparation step.

**Sanity run first** (config S memorizing the tiny `{3,3}` subspace, ~20 min on CPU;
if this doesn't reach ≥97% accuracy something is broken):

```sh
uv run python -m nmm.train --db ../db --config S --only-pairs 3,3 --samples 20000000 --out checkpoints/sanity_S.pt
```

**Real training** (wants the full database and a GPU; `--device cuda` or `mps`):

```sh
uv run python -m nmm.train --db ../db --config M --samples 300000000 \
    --device cuda --out checkpoints/model_M.pt
```

Useful flags (see `python -m nmm.train --help` for all): `--config S|M|L` picks the
model size (~74k / ~0.54M / ~1.8M parameters), `--samples` is the total training
budget (a cosine LR schedule is fit to it), `--batch-size` (default 8192),
`--num-workers` for the data loader (throughput is loader-bound on CPU,
~15–25k samples/s single-threaded), `--only-pairs "3,3;4,4"` restricts subspaces.
Checkpoints (`*.pt`) are gitignored — every model here is reproducible from the
database.

**Logging**: by default metrics only go to stdout. Pass `--logdir runs/<name>` to
also write TensorBoard event files (same stats as the console — train loss/accuracy/
cross-entropy/depth-MSE/LR/throughput plus the validation metrics, keyed by samples
seen so runs with different batch sizes stay comparable), then view with:

```sh
uv run tensorboard --logdir runs
```

The `tensorboard` package is in the `dev` extra; without `--logdir` it is never
imported. `runs/` is gitignored.

The shipping-model recipe — an S/M/L × sample-budget grid on the full database,
judged on blunder rate and soak matches per `../design-nn.md` §8 — is milestone N7
and has **not** been run yet; `RESULTS-nn.md` records a demo-scale config-M
checkpoint (96.7% state accuracy, 0 blunders in 3000 sampled moves, but 6% soak-game
losses at 1-ply) and names the open experiment: does shallow search close the
soak-loss gap?

## Evaluating a checkpoint

Move quality is the metric that matters (blunder rate first, optimal-move rate
second, state accuracy last). `nmm/evaluate.py` is a library, not a CLI:

```python
import torch
from nmm.db import Database
from nmm.model import NmmNet
from nmm.evaluate import NetEvaluator, evaluate_move_quality, run_soak

db = Database("../db")
ckpt = torch.load("checkpoints/model_M.pt", map_location="cpu", weights_only=True)
model = NmmNet(hidden=ckpt["hidden"], n_blocks=ckpt["n_blocks"])
model.load_state_dict(ckpt["state_dict"])

ev = NetEvaluator(model, tta=True)          # tta = 16-way symmetry averaging
print(evaluate_move_quality(db, ev, n_samples=3000))       # blunder / optimal-move rates
print(run_soak(db, ev, n_games=150, search_depth=1))       # full games vs. the exact database player
```

`search_depth > 0` tests the actual deployment stack (TTA at root + shallow negamax,
matching `../web/src/engine.ts`) instead of the raw 1-ply move choice.

## Playing against it (browser)

The web demo (`../web/`) is a dependency-free TypeScript runtime: rules, symmetry,
a hand-rolled forward pass, and search — no onnxruntime, no server-side engine.

```sh
# 1. Export the checkpoint to the web runtime's format (writes model.bin,
#    model.json, model.onnx, and golden test vectors):
cd ml
uv run python -m nmm.export model --checkpoint checkpoints/model_M.pt --out ../web/export

# 2. Build the TypeScript and serve the repo's web/ directory:
cd ../web
npm install
npm run build
node scripts/serve.mjs        # serves on http://localhost:8834
```

Then open **http://localhost:8834/public/index.html**. You play White by clicking
points; the engine answers with the exported network (heuristic during placement,
model + search once both sides are placed). The page also shows the training-tool
view: live win/draw/loss bars for every candidate move.

Parity between Python and the browser is gated: `npm run verify-golden` checks the
TS forward pass against PyTorch golden vectors (after `nmm.export model`), and
`npm run verify-fixtures` checks geometry and 10,000 legal-move traces (after
`nmm.export geometry` / `nmm.export rules-trace` into `../web/fixtures`).

Note this is playing against the *network*. To play against the exact database
itself (perfect play by construction, requires the full ~18 GB database locally),
use the Rust side: `ninemm play` / `ninemm serve` — see
[`../getting-started.md`](../getting-started.md).

## Directory map

| Path | What it is |
|---|---|
| `nmm/board.py`, `symmetry.py`, `ranking.py` | Board geometry, 16-element symmetry group, combinatorial indexing |
| `nmm/db.py` | Reader for the solver's on-disk format ([`../readme-database.md`](../readme-database.md)) |
| `nmm/movegen.py`, `consistency.py` | Independent move generator + forward-consistency gate against the database |
| `nmm/features.py`, `dataset.py` | Input featurization and the streaming sampler |
| `nmm/model.py`, `train.py` | The network (S/M/L configs) and training CLI |
| `nmm/evaluate.py` | Blunder rate, optimal-move rate, soak matches vs. the exact player |
| `nmm/export.py` | ONNX + raw-blob export, golden vectors, geometry/rules fixtures for web parity tests |
| `tests/` | Pytest gates, one file per module (`-m slow` for the expensive ones) |
