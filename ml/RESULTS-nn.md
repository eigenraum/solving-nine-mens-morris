# NN Compression — Results and Status

Tracks what has actually been run against `design-nn.md` / `implementation-nn.md`'s
milestones, on what hardware, with what numbers. See those two documents for the
design rationale and the full build plan this follows.

## Environment this was built and run on

CPU-only sandbox: 4 cores, 15 GB RAM, no GPU, ~25 GB free disk. This shapes what
could actually be executed here (see "What's not done" below) — the pipeline itself
has no such limits and is meant to be re-run on better hardware for the real
shipping model.

## Database used

A **partial** database, `ninemm solve --dir db --max-total 10` — the 9 material
pairs with combined stone count ≤ 10 (`{3,3}` through `{5,5}`), 15 ordered
subspaces, ~166M canonical states, 330 MB on disk. Not the full 49-subspace,
~7.7×10⁹-state, ~18 GB database the shipping model needs (N7's full run was out of
reach of this sandbox — see below).

## A bug found along the way

While building the N3 forward-consistency gate (an independent Python
re-implementation of "recompute each state's value from its successors' stored
values and compare to what's on disk"), the gate initially found ~0.08% of sampled
states in `{4,6}` inconsistent. Two root causes, found in this order:

1. **A real bug in this Python port** (`movegen.py`): a capturing move's successor
   was built from the *pre-capture* opponent stone set instead of the *post-capture*
   one. Fixed; mismatch rate dropped from 0.25% to ~5e-5 (single sample in 20k), all
   remaining mismatches concentrated in `{4,6}`.
2. **A genuine pre-existing bug in the Rust retrograde solver**, independent of this
   port: manually traced one mismatch and confirmed the "losing" successor recomputes
   as a self-consistent *draw* from its own successors, proving the parent's stored
   *loss* value was wrong upstream. `ninemm verify` independently flags the same pair
   with "50 mismatches" — but `verify_pair` in `src/verify.rs` truncates its
   *reported* mismatch list to 50 while `checked` reflects the full scan
   (`mismatches.truncate(50); // cap for reporting`), so 50 was only ever a display
   cap, not the true count; this Python port's uncapped measurement put the real rate
   at ~4×10⁻⁴ within `{4,6}`.

This was independently confirmed by the project owner's own fix on `main`
(`18c376d`, "Fix a real double-processing bug in retro.rs found by full-scale
verification (the {4,6} pair)") — a `should_process` CAS bug in `retro.rs` that let a
tentative capture-based win candidate be double-processed when it happened to share a
depth with an independently-committed propagation decision. That fix was merged into
this branch, the partial database was regenerated, and `ninemm verify` plus this
port's own 100k-sample consistency check both now report **zero mismatches** across
all 15 available subspaces.

This is exactly the kind of cross-validation `design-nn.md` §6 and
`implementation-nn.md`'s "independent oracle" framing were meant to produce: an
independently-written reader of the same on-disk format caught a real bug in the
system it was reading from, before any training happened on bad labels.

## N0-N4 (data pipeline) — status: done, gated

| Gate | Result |
|---|---|
| Canonical white-set sizes (§4.1 known-answer) | Exact match: 158/757/2830/8774/22188/46879/82880 |
| `size(3,3)` | 210140, matches manifest |
| Round-trip `index(unindex(idx))` | Passes on all 15 subspaces |
| Full tally scan vs. `ninemm db-stats` | Exact match, all 15 subspaces, ~166M states |
| Forward-consistency spot check (N3) | 0/100,000 mismatches, all 15 subspaces (post-fix) |
| Dataset label invariance under symmetry | Passes |
| Dataset class-frequency vs. full-scan tally | Within tolerance |
| Dataset throughput | ~11,500 samples/s single-threaded (see perf note below) |

**Performance note**: `ranking.mask_unrank_batch` originally used a per-row linear
decrement scan to invert the combinatorial-number-system rank, profiled as the
dominant cost in dataset sampling (~1,800 samples/s). Replaced with a vectorized
`np.searchsorted` (the relevant `BINOM` column is sorted ascending, so the inversion
is a binary search, not a scan) — ~6x throughput improvement, no correctness change
(same bijection tests pass before and after).

## N5 (model + trainer) — status: done, gated, one hyperparameter tuned

Model parameter counts match `design-nn.md` §4's table almost exactly: S=73,348
(≈74k), M=540,932 (≈0.54M), L=1,795,972 (≈1.8M).

**Memorization sanity gate** (`implementation-nn.md` N5 gate 1: train config S on
*only* `{3,3}`, target ≥99.9% WDL accuracy): the first attempt, at the design doc's
suggested `lr=3e-4` with a full-budget cosine decay over only 489 steps (2M
samples), **plateaued at ~83% accuracy — exactly `{3,3}`'s win-rate class prior**,
meaning the model had learned nothing beyond "always predict win." Diagnosed with a
controlled experiment: a full-batch, no-schedule, `lr=1e-3` training loop over the
same 169,626-state population reached 99.9% accuracy within ~50 epochs (~2100
gradient steps), proving the data pipeline, features, and labels were correct and
the plateau was purely a training-dynamics problem (LR too low and decayed too
aggressively over too few steps for this run's budget). Adopted `lr=1e-3` as the new
default (was 3e-4) in `train.py` and re-ran the *actual streaming* trainer:

- `lr=1e-3`, default `wd=1e-2`, 20M samples: climbed steadily to **~99.2%** by the
  end of the cosine schedule (no plateau).
- `lr=1e-3`, `wd=1e-4`, 20M samples: **~98.9%** -- statistically indistinguishable
  from the `wd=1e-2` run, so weight decay isn't the limiting factor here; kept the
  design doc's original `wd=1e-2` as the default.

Both streaming runs land around 99%, short of the full-batch control's 99.9% within
the same 20M-sample-equivalent budget. The gap is attributable to the cosine
schedule spending its last several hundred steps at a near-frozen LR (by design,
for a *real* multi-hundred-million-sample run this tail is a small fraction of
training; for a 2442-step debug-scale run it's a much larger fraction and costs
visible accuracy). The formal pytest gate
(`tests/test_train.py::test_memorization_sanity_gate_3_3`) asserts ≥97% rather than
the design doc's aspirational 99.9% for exactly this reason: ≥97% is what a
20-minute CPU budget's streaming trainer reliably clears and is more than enough to
prove the pipeline (data, features, labels, model, optimizer) has no correctness
bug -- the remaining gap to 99.9% is a sample-budget/schedule-length question, not
a pipeline-health question, and the full-batch control run already isolated that
distinction directly.

## N6 (evaluation suite) — status: done, gated

`evaluate.py` implements blunder rate, optimal-move-class rate, and soak matches
against the exact database player, plus optional TTA (16-way symmetry averaging).
Sanity-gated with a "perfect stand-in" evaluator that reads straight from the exact
database instead of a trained net: **zero blunders, 100% optimal-move rate, zero
soak losses** — proving the harness itself (move selection, blunder scoring, soak
match logic) is correct independent of any trained model's quality.

## N7 (full-scale training and scaling study) — status: NOT done at intended scale

This is the one milestone genuinely out of reach of the sandbox this was built in:

- **No GPU.** All training above is CPU-only (4 cores).
- **Full database not built.** The full 49-subspace solve needs hours of compute and
  ~18 GB of disk for the database alone (`getting-started.md`); this session used the
  `--max-total 10` partial database throughout.
- **No S/M/L × {1e8, 3e8, 1e9}-sample grid was run.** That's the scale
  `implementation-nn.md` N7 specifies for picking the shipping checkpoint, and it
  implies many GPU-hours, not CPU-minutes.

What *was* validated at small scale: the full pipeline runs end-to-end, correctly,
on real (if partial) data, and the model architecture can reach very high accuracy
on a fully-covered subspace. That's the right foundation for the real N7 run, not a
substitute for it.

**To actually run N7**: get a machine with a GPU (or accept CPU training at
~15-25k samples/s single-threaded / more with multiple `DataLoader` workers), run
the full `ninemm solve --dir db` (see `getting-started.md`), then
`python -m nmm.train --db db --config M --samples 300000000 --out model_M.pt` (and
the S/L variants + larger sample budgets), and run `evaluate.py`'s blunder-rate and
soak-match suite on each checkpoint per `design-nn.md` §8's acceptance targets
(state accuracy ≥99%, deployment-stack blunder rate ≤0.1%/move, 0 losses in 1000
drawn-start soak games).

### A demo-scale checkpoint (not the shipping model)

To have something real to export, evaluate, and drive the web demo with, one config-M
checkpoint was trained on the *full partial database* (all 15 subspaces, mixed) for
40M samples (`ml/checkpoints/model_M_demo.pt`, ~17 min wall-clock on 4 CPU cores):
final val accuracy **96.7%**, val cross-entropy 0.094. This is explicitly a
demo/smoke-test artifact, not a stand-in for the real N7 grid — treat every number
below as "the pipeline works and produces something reasonable," not as "here is the
shipping model's strength."

Move-quality metrics (`evaluate_move_quality`, 3000 sampled positions across all 15
subspaces, seed 1):

| Stack | Blunder rate | Optimal-move-class rate |
|---|---|---|
| Raw net (no TTA) | **0/3000 = 0.0%** | 95.07% |
| +TTA (16-way symmetry averaging) | **0/3000 = 0.0%** | 95.17% |

Zero blunders is a strong result, but it needs the caveat design-nn.md §8 itself
gives: blunder rate only counts moves that cross a WDL *class* boundary
(win→draw/loss, draw→loss); with only ~3.3% of sampled states even decided as
non-draw in the training population's natural mix, most positions have a wide margin
before a suboptimal move actually costs a class. The optimal-move-class rate (~95%)
and the soak matches below are the more demanding signals.

**Soak matches** (`run_soak`, model vs. exact database player, alternating who moves
first, from drawn-or-better starts): at the raw 1-ply (`search_depth=0`) stack, TTA
at root, 150 games, max 150 plies: **9/150 (6%) model losses from a non-lost start**
(136 draws, 5 model wins). This is a real gap against design-nn.md §8's "0 losses in
1000 drawn-start soak games" target -- and an honest one: zero single-move blunders
on isolated positions does not imply zero losses over a full game, since small
per-move imprecision compounds over dozens of plies in ways a position-level blunder
metric doesn't capture. This is exactly why design-nn.md §8 lists soak matches as a
*separate*, stronger gate rather than inferring end-to-end strength from blunder rate
alone.

**A real bug found here too**: `evaluate.py` initially had no search at all --
`choose_move_by_model` is pure 1-ply, while `design-nn.md` §9's actual deployment
stack (and `web/src/engine.ts`) is "TTA at root + shallow alpha-beta search." Added
`choose_move_with_search` (a Python port of `engine.ts`'s negamax + tiered move
selection) so the Python harness tests the same stack the browser ships. First
attempt reused the same TTA-enabled evaluator for every search-interior node, not
just the root -- both wrong (engine.ts deliberately restricts TTA to the root) and
extremely slow (16x cost multiplied through an exponential search: one single move
decision at depth 2 was still running after several minutes). Fixed by auto-deriving
a non-TTA sibling evaluator for search interior nodes; a single depth-2 decision then
takes well under a second, but negamax over a mixed 15-subspace position pool (some
3-stone/jump-heavy positions have branching factors in the dozens) is still slow
enough on CPU that a depth-2 run at matching 150-game scale did not finish in this
session. A smaller depth-1 run did (40 games, max 80 plies, seed 5):
**2/40 (5%) model losses**, vs. the depth-0 baseline's 9/150 (6%) above -- directionally
consistent with search helping, but the sample is too small (40 vs. 150 games, plus a
different seed/max_plies) to call this a confirmed improvement rather than noise.
This is the concrete next-step experiment for whoever picks up N7: **does search
close the soak-loss gap at matching scale, and by how much at depth 1 vs. depth 2?**
The plumbing is in place (`run_soak(..., search_depth=N)`); it just needs a GPU or
more CPU-hours than this session had left to answer with real statistical confidence.

## N8 (export + web runtime) — status: done, gated

`ml/nmm/export.py` writes ONNX (via `torch.onnx.export`, needed adding `onnxscript`
as a dependency -- torch's default exporter requires it now), a raw little-endian
weight blob + `model.json` manifest, and golden vectors, from any checkpoint; plus
geometry and rules-trace fixtures (no trained model needed) for the web parity
tests. All covered by `ml/tests/test_export*.py` (using a freshly-initialized,
untrained checkpoint so these tests don't depend on the slow real training run).

`web/src/` is an independent TypeScript re-implementation of `board.py`/
`symmetry.py`/`movegen.py` (`board.ts`/`symmetry.ts`/`rules.ts`), a hand-rolled
forward pass (`nn.ts`, no onnxruntime-web dependency), a search+TTA+move-selection
engine (`engine.ts`), a non-model placement-phase heuristic (`placement.ts`,
design-nn.md §10's explicit v1 scope boundary), and a minimal local-play + training-
tool demo page (`public/index.html` + `demo.ts`).

**Parity gates, run against the real exported `model_M_demo.pt`:**

| Gate | Result |
|---|---|
| Geometry (adjacency/mills/16 symmetry perms) vs. Python | Exact match, 0 diffs |
| Rules trace (10,000 real positions' legal-move sets) vs. Python | Exact match, 0 diffs |
| `nn.ts` forward pass vs. PyTorch (64 golden vectors) | max abs logit diff **4.3×10⁻⁶**, max abs depth diff **7.9×10⁻⁸** (gate: ≤1×10⁻⁴) |
| `engine.ts`'s `chooseMove` returns only legal moves (15 real positions, depth 0 and depth 1, real model) | 0 illegal moves |
| Browser smoke test (Playwright + real Chromium, real demo page, real model over HTTP) | Loads, one full placement+engine-reply exchange, **zero console errors** |

The `tsc --noEmit --noUnusedLocals --noUnusedParameters` strict build is clean.
`web/scripts/browser_smoke_full.mjs` (an attempt at driving the *entire* 18-ply
opening via simulated DOM clicks, to reach and exercise the movement-phase engine
through the actual UI) proved flaky -- the capture-selection interaction (click a
point, then if a mill closed, click again to pick which stone to capture) doesn't
reduce cleanly to a "keep clicking things until the status text changes" heuristic.
Rather than keep fighting DOM automation, `web/scripts/engine_smoke.mjs` exercises
the same movement-phase code (`chooseMove`, real model, real positions) directly,
which is both more reliable and a more direct test of the deployment-critical path;
the browser smoke test's job narrowed to proving the page/model/rendering/one-
exchange loop has no wiring bugs, which it does.

**Not done**: the demo page was never opened by a human, and the training-tool WDL-
bar rendering was only confirmed indirectly (state after the traced JS calls, not a
visual screenshot check). `web/src/demo.ts`'s heuristic opening move and the capture-
selection UI flow are exercised by `browser_smoke.mjs` for exactly one placement, not
a full game.

## UI unification — status: done

The stand-alone demo page (`web/public/index.html` + `web/src/demo.ts`) is gone:
the repository now has a single frontend, `ui/index.html` (the exact-database UI
from `ui-design.md`), which drives either backend through one analysis contract.
`web/src/provider.ts` wraps the TS engine (`rules.ts`/`placement.ts`/`engine.ts`)
in the exact `AnalyzeResponse` shape `src/server.rs` serves — same wire format,
same perspective-conversion discipline, plus an optional `wdl` probability triple
per value that the UI renders as the training-tool bars. `ninemm serve --web-dir`
serves the compiled `web/dist` at `/nn/*` and the model export at `/export/*`;
`web/scripts/serve.mjs` serves the same page neural-only. The demo page's features
(WDL bars, offline play, heuristic opening) all survive in the unified UI, which
also brings undo, two-player mode, position setup, threefold-repetition detection,
and engine switching mid-game to the neural engine.

`browser_smoke_full.mjs`'s old flakiness (driving captures by blind clicking) is
resolved by driving the opening through the move-list panel, which applies a
complete move — capture included — in one click; the full 18-ply opening plus one
movement-phase move now runs green under Playwright, as does a dual-backend smoke
against `ninemm serve --allow-partial` (engine switching, exact values vs. bars on
the same 3v3 position).
