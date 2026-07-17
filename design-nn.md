# Neural Compression of the Nine Men's Morris Database — Design

Goal: train a **lightweight PyTorch network by supervised learning on the solved
database** (`readme-database.md`) so that it acts as a *lossy-compressed* stand-in for
the full ~18 GB artifact. End state: the network ships inside a browser page (WebAssembly
or plain JS/TS — it is small enough for either) and powers

1. a **fully local opponent** that plays perfectly or near-perfectly with no server and
   no multi-gigabyte download, and
2. an **interactive training tool** that shows a live evaluation (win/draw/loss
   probabilities, estimated distance-to-win) for every candidate move.

This document makes the design decisions; `implementation-nn.md` is the step-by-step
build plan (written to be executable by a less capable model without further design
work). Companion docs: `design.md` (the solver), `readme-database.md` (the on-disk
format this project consumes — the ML side treats the database directory as its input
artifact and never touches solver internals).

---

## 1. Framing: this is compression, not generalization

The database stores the exact value of every mid/endgame state: **~9.05 × 10⁹ index
slots** (≈ 7.7 × 10⁹ real states after symmetry; the rest is deliberate indexing slack),
2 bytes each ≈ 18 GB. A ~1 MB network is therefore a **~10,000× lossy compression** of
a *finite, fully enumerable* function. Three consequences that shape everything below:

- **Overfitting is not a failure mode — it is the objective.** The training
  distribution *is* the deployment distribution (the finite set of database states).
  We may train on (a sample of) the entire population. A held-out split exists only to
  *estimate fidelity over the full population* cheaply, not to measure generalization
  to unseen distributions. Memorization capacity is welcome; regularization is used
  only insofar as it improves total-population fidelity per parameter.
- **Perfection cannot be guaranteed by the raw network.** A lossy code has errors by
  construction. "Perfect opponent" is achieved *in layers*: the network's raw fidelity,
  ×16 symmetry test-time averaging, a shallow (2–4 ply) search using exact terminal
  rules with network leaf evaluations, and (optional extension) exact bit-packed
  tables for the smallest subspaces. We define measurable targets (§8) instead of
  claiming perfection.
- **The right metric is move quality, not state accuracy.** A state misclassification
  only matters if it changes which move gets picked, and only *really* matters if the
  picked move changes the achievable game-theoretic outcome (a *blunder*). All
  evaluation is designed around blunder rate first, optimal-move rate second, raw
  state accuracy last (§8).

**Scope**: like the database itself, the core model covers the **movement/endgame phase
only** (both sides fully placed, 3–9 stones each). The placement phase (plies 1–18) is
a staged extension (§10); until it exists, the web opponent plays the opening with a
book/heuristic — acceptable because optimal opening play is highly drawish (see
`readme-database.md`, Scope note).

## 2. What the network predicts

**A value network only. No policy head.**

- **Primary head — WDL**: 3-way softmax over {loss, draw, win} *for the side to move*,
  matching the database's side-to-move convention exactly (`readme-database.md` §2).
  Trained with cross-entropy against the exact stored class.
- **Auxiliary head — depth**: one scalar, regression on the stored depth `d`
  (normalized `d/255`), masked to decided (non-draw) states. Two purposes: (a) it is
  needed at play time to order moves *within* a class — win as fast as possible, lose
  as slowly as possible, mirroring the database play rule (`readme-database.md` §5);
  (b) as an auxiliary task it sharpens the shared representation. Depth here is the
  database's DTC-flavored depth (plies to conversion/terminal within the material
  pair, `design.md` §4) — we predict what is stored, we do not re-derive any other
  depth notion.

Why no policy head: move selection falls out of the value head by **1-ply lookup**,
exactly how a consumer uses the real database — enumerate legal moves, evaluate each
successor from the opponent's perspective, pick the move whose successor is worst for
the opponent (successor classified loss-for-mover with minimal predicted depth ▸ draw
▸ win-for-mover with maximal predicted depth; a capture that drops the opponent below
3 stones is an immediate win, handled by rule, not by the net). This keeps the network
smaller, keeps train-time and play-time semantics identical, and gives the training
tool a per-move evaluation for free. Legal-move sets are small (typically < 30, worst
case a few times that with 3-stone jumps), and the net is tiny, so evaluating every
successor is trivially cheap even in WASM.

## 3. Input representation

A flat vector of **52 floats** (all in {0,1} or [0,1]):

| slice | content |
|---|---|
| 0–23 | mover stones, one per board point (point numbering per `readme-database.md` §1) |
| 24–47 | opponent stones |
| 48 | mover stone count / 9 |
| 49 | opponent stone count / 9 |
| 50 | mover has exactly 3 stones (may jump) |
| 51 | opponent has exactly 3 stones |

Features 48–51 are derivable from 0–47; they are included because they gate rule
changes (jumping) and material subspace identity, and giving them explicitly is far
cheaper than making a small net learn popcounts. No color feature exists — the
mover/opponent normalization already removes color, same as the database.

Hand-engineered mill/adjacency features are deliberately **not** included in v1: the
16 mill masks are a fixed sparse structure the first linear layer can represent
directly, and every extra feature has to be reimplemented bug-free in the web runtime.
Revisit only if the scaling study (§7) hits a fidelity wall.

## 4. Architecture

**A plain residual MLP.** Board-graph-aware architectures (GNN over the 24-point
adjacency, tiny transformers over point tokens) were considered and rejected for v1:
they buy sample-efficiency we don't need (labels are unlimited and free), and cost
deployment complexity we can't afford (the entire inference stack must be
reimplemented in the browser and kept bit-compatible).

```
input 52
→ Linear 52→H, ReLU
→ B × [ Linear H→H, ReLU, Linear H→H, add skip, ReLU ]
→ heads: Linear H→3 (WDL logits) ; Linear H→1, sigmoid (depth/255)
```

Three reference configurations (the scaling study in `implementation-nn.md` M7 picks
the shipping one; M is the default assumption):

| config | H | B | params | fp32 | fp16 | int8 |
|---|---|---|---|---|---|---|
| S | 128 | 2 | ~74 k | 0.30 MB | 0.15 MB | 0.08 MB |
| M | 256 | 4 | ~0.54 M | 2.2 MB | 1.1 MB | 0.55 MB |
| L | 384 | 6 | ~1.8 M | 7.2 MB | 3.6 MB | 1.8 MB |

Even L at fp16 is a smaller download than a typical hero image; the binding budget is
inference latency inside a browser search (§9), not weight size. No normalization
layers (batch-independent inference, trivial to port); if training instability appears
at L scale, prefer LayerNorm and accept the porting cost.

## 5. Symmetry

The value function is invariant under the 16-element board automorphism group
(`readme-database.md` §3). Two standard options: canonicalize inputs (forces the web
runtime to port canonicalization) or make the net approximately invariant via
augmentation. **Decision: augmentation at training time** — every sampled state is
transformed by a uniformly random one of the 16 symmetries before featurization
(label unchanged) — plus **optional test-time augmentation (TTA)** at inference:
average WDL logits over all 16 transforms. TTA is a pure-runtime knob: on for
root-move decisions and the training tool's displayed evaluations, off inside search
nodes where throughput matters. The web runtime then needs only the 16 precomputed
point permutations (a 16×24 byte table), not canonicalization logic.

## 6. Training data

**Source**: the `db/` directory produced by `ninemm solve`, read directly in Python
(numpy memmap of the little-endian `u16` arrays). The reading code is a from-scratch
port of `readme-database.md` §§1–5 — it shares no code with the Rust solver, which
makes it an independent implementation that must pass its own verification gates
(`implementation-nn.md` M2–M3) before any training happens. No Rust-side exporter is
needed; the format spec is sufficient.

**Sampling**: draw slots i.i.d. — subspace chosen proportional to slot count, slot
index uniform within the subspace, position recovered by unranking. Two filters:

- **Wasted slots** (`readme-database.md` §4.3) must be rejected by checking that the
  unranked position is its own canonical form. This is critical and easy to get
  silently wrong: wasted slots physically contain `0xFFFF`, which *decodes as "draw"*
  — training on them poisons the draw class with meaningless states. Never filter by
  value; filter by canonicality.
- **Unreachable states are kept.** The solver assigns them correct game-theoretic
  values (retrograde analysis solves every state in a subspace), they are harmless,
  and reachability filtering was explicitly out of scope for the solver too
  (`design.md` §3). If fidelity on reachable states ever needs a boost, importance
  reweighting is a later knob, not a v1 feature.

**Split**: deterministic hash of `(subspace, index)` → ~0.5 % validation, ~0.5 % test,
rest trainable. Per §1, the split measures population fidelity; it is fine that the
model sees "similar" states across the split — all states are the deployment set.

**Scale**: one full epoch is ~7.7 × 10⁹ states — unnecessary. Budget on the order of
10⁹ sampled states for the shipping run (≈13 % of the population, more with
augmentation counted); the scaling study measures the fidelity-vs-samples curve and
extends the budget only if it is still improving. The loader is a streaming
`IterableDataset` doing vectorized unranking in numpy; nothing is ever fully
materialized in RAM.

**Class balance**: W/D/L proportions vary enormously across subspaces (e.g. {3,3} is
~83 % wins). v1 trains on the natural distribution with plain cross-entropy — that is
the distribution play actually visits, and blunder-rate metrics (§8) will reveal if
rare-class fidelity (e.g. draws inside win-heavy subspaces) is the weak point. Class
or subspace reweighting is a tuning knob held in reserve.

## 7. Training procedure

Loss: `CE(WDL) + λ · masked-MSE(depth)`, λ = 0.25 initially (depth is an auxiliary —
it must not dominate). AdamW, cosine decay, bf16/fp32 as hardware allows, batch 8192
(the model is tiny; throughput is loader-bound, so big batches are free). Full
hyperparameters, schedules and the S/M/L × sample-budget scaling study are specified
in `implementation-nn.md` M5/M7. A deliberate sequencing rule from the solver project
applies here too: **the pipeline is validated end-to-end on a partial database first**
(`ninemm solve --max-total 10` finishes in minutes and yields 9 pairs) before any
full-scale run.

## 8. Evaluation — the metrics that matter

Measured on held-out states (state-level) and freshly sampled positions (move-level),
always against the exact database:

1. **Blunder rate** (primary): fraction of positions where the model's chosen move
   worsens the game-theoretic value actually achievable (win→draw, win→loss,
   draw→loss). Report per material subspace and overall; also report the
   *game-level* compound: expected number of blunders over a synthetic 40-move game
   path.
2. **Optimal-move rate**: chosen move is in the database-optimal set (same value
   class; for wins additionally minimal depth — reported separately, since picking a
   win-in-9 over a win-in-7 is not a blunder).
3. **Soak matches**: the model (with its deployment-time stack: TTA + shallow search)
   plays complete movement-phase games against the exact database player from
   randomized won/drawn openings; a model loss from a non-lost start is a counted
   failure. This is the end-to-end gate the web opponent's strength claim rests on.
4. **State fidelity**: WDL accuracy + confusion matrix, depth MAE on decided states —
   diagnostic only.

**Acceptance targets for shipping v1** (goals to steer by, not guarantees): raw-net
state accuracy ≥ 99 %; blunder rate of the full deployment stack (TTA + 3-ply search)
≤ 0.1 % per move, and zero losses in 1,000 soak games from drawn starts. If the M
config misses these, the scaling study escalates (L config, larger sample budget,
class reweighting) before any architecture rethink.

## 9. Deployment

**Export**: two artifacts from one checkpoint — (a) ONNX, for tooling/interop; (b) a
**raw little-endian weight blob + JSON manifest** (layer shapes, activation spec,
feature spec, normalization constants) consumed by a **hand-rolled TypeScript
inference routine** (~150 lines of matmuls). Rationale: onnxruntime-web costs ~1 MB+
of WASM and a large API surface to run what is, for config M, ~1 MFLOP per
evaluation; a hand-rolled forward pass is trivially auditable and is verified against
PyTorch to ≤1e-4 max abs logit difference on a golden vector set (exported alongside
the weights). "WebAssembly or the like" from the project goal is thus satisfied by
plain JS/TS first; a Rust→WASM port of the same forward pass (SIMD) is a drop-in
upgrade if search throughput demands it.

**Quantization**: ship fp16 by default (halves size, no measurable fidelity change
expected); int8 post-training quantization is evaluated in M7 and adopted only if the
move-level metrics (§8) are unchanged.

**The browser opponent** = TS rules engine (movement-phase move generation per
`readme-database.md` §6 — reimplemented, with the same spot-check-against-database
verification idea used for the Python port) + αβ search of depth 2–4 with exact
terminal rules at interior nodes and network evaluations at leaves + TTA at the root.
Budget: config M ≈ 1.1 MFLOP/eval ⇒ a 3-ply search of a few thousand leaves ≈ low
single-digit GFLOP ≈ sub-second in modern JS engines, comfortably interactive.

**The training tool** reuses the same stack: for every legal move show the WDL
probability triple and predicted depth of the resulting position (flipped to the
human's perspective), flag moves whose predicted class is worse than the best
available — i.e. exactly the §2 move-selection data, rendered instead of argmaxed.
The softmax probabilities double as a graded "how confident is the compressed model"
signal that the ternary database cannot provide.

## 10. Staged extensions (explicitly out of v1 scope)

- **Opening/placement phase model.** Extend the input with stones-in-hand counts (2
  extra features) and train on exact placement-phase labels generated by the existing
  Rust opening search (`opening.rs` alpha-beta + database probing) — this requires a
  small new Rust export command (sample placement states, label each by searching to the
  ply-18 boundary) and is the only piece of the ML plan that touches Rust. Until
  then: opening book/heuristic in the web app, switch to the net at ply 19.
- **Hybrid exact endgames**: ship 2-bit-packed exact WDL for the smallest subspaces
  (a few MB covers all pairs up to ~10 total stones) so late endgames are provably
  perfect; net handles the rest. Compute exact sizes before committing.
- **Ultra-strong play** (prefer moves maximizing opponent error chances among
  equal-value options) — same status as in the solver: out of scope.

## 11. Risks

| Risk | Mitigation |
|---|---|
| Python DB reader silently disagrees with the Rust spec (index/symmetry bug) | M2–M3 verification gates: canonical-table size checksums, `db-stats` tally match, million-state minimax forward-consistency spot check — all *before* training (same "independent oracle" philosophy as `design.md` §8) |
| Wasted `0xFFFF` slots leak into training as fake draws | canonicality filter in the sampler + a measured wasted-slot rate reported per subspace |
| Net looks accurate but blunders in play | move-level metrics and soak matches are the acceptance gates, not state accuracy |
| Draw class starves in win-dominated subspaces | per-subspace confusion matrices; reweighting knob held in reserve |
| TS/WASM inference drifts from PyTorch | golden-vector parity test in CI; single shared weight blob |
| Browser search too slow with TTA | TTA at root only; S config for interior nodes; Rust→WASM SIMD port as escalation |
| Full DB (18 GB, hours to solve) unavailable on the ML machine | entire pipeline runs on `--max-total 10` partial DBs; full run needed only for the shipping model |

## 12. Technology

**PyTorch** (training, stable ≥2.x), **numpy** for the vectorized reader/sampler,
**pytest** for the verification gates, **onnx** for export, plain **TypeScript** for
deployment inference. Python ≥3.11, managed with `uv`; the ML code lives in `ml/` as
an installable package (`ml/nmm/…`), fully separate from the Rust crate — the
database directory is the only interface between them.
