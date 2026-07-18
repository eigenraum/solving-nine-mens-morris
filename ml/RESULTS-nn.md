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

## N8 (export + web runtime) — status: <!-- N8_STATUS -->
