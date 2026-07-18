# Neural Compression — Implementation Guide

Step-by-step build plan for `design-nn.md`. This guide is written to be executable
**without further design decisions**: every formula you need is restated here in full
(cross-references to `readme-database.md` are for authority, not required reading
order), every milestone ends with a concrete verification gate including known-answer
numbers, and known traps are flagged with ⚠ at the point where they bite. Follow the
milestones in order; **do not start a later milestone until the earlier gate passes**
— this project's Rust side caught every one of its serious bugs at verification gates,
never by code inspection (`readme-agent.md`, "Bugs found"), and the same discipline
applies here.

Conventions used throughout:
- "Position" always means a pair of disjoint 24-bit integer masks `(mover, opponent)`
  — bit `p` set means a stone on point `p`. Values are always **for the side to
  move**. There is no color anywhere in this pipeline.
- All multi-byte data on disk is **little-endian**.
- `C(n, k)` is the binomial coefficient, `C(n, k) = 0` when `k < 0` or `k > n`.

## Project layout (create in milestone N0)

```
ml/
  pyproject.toml          # package "nmm", deps: numpy, torch, pytest, onnx
  nmm/
    __init__.py
    board.py              # geometry: adjacency masks, mill masks
    symmetry.py           # 16 permutations, apply, canonical checks
    ranking.py            # binomials, subset rank/unrank, canonical white sets
    db.py                 # Database reader: memmap, decode, lookup, sampling
    movegen.py            # movement-phase legal moves
    consistency.py        # forward-consistency spot check (the big gate)
    dataset.py            # streaming torch IterableDataset
    model.py              # residual MLP (configs S/M/L)
    train.py              # training loop CLI
    evaluate.py           # state metrics, move metrics, soak matches
    export.py             # ONNX + raw weight blob + golden vectors
  tests/                  # pytest; one file per module, named test_<module>.py
  cache/                  # gitignored: canonical-set .npz cache
web/                      # milestone N8: TS inference + rules + demo page
```

Add `ml/cache/` and `ml/**/__pycache__` to the repo `.gitignore`. Keep the Rust crate
untouched — the `db/` directory is the only interface.

---

## N0 — Prerequisites and scaffolding

1. Build the solver and produce a **partial** database for development:
   ```sh
   cargo build --release
   ./target/release/ninemm solve --dir db --max-total 10
   ./target/release/ninemm verify --dir db
   ./target/release/ninemm db-stats --dir db > db-stats-partial.txt
   ```
   `--max-total 10` solves the 9 material pairs with ≤10 total stones in minutes.
   Save the `db-stats` output — N2's gate diffs against it. The **full** solve
   (hours, 18 GB disk, ≥16 GB RAM — see `getting-started.md`) is only needed from
   N7 onward.
2. Set up `ml/` with `uv init` + the layout above; empty modules, one trivial passing
   test, `uv run pytest` green.

**Gate**: `ninemm verify --dir db` exits 0; `uv run pytest` passes.

## N1 — Geometry and symmetry (`board.py`, `symmetry.py`)

### Geometry

Point `p = ring*8 + i`, ring ∈ {0 outer, 1 middle, 2 inner}, i ∈ 0..8.

- Adjacency: `ring*8+i ↔ ring*8+((i±1) mod 8)`; additionally, for **odd i only**,
  `0*8+i ↔ 1*8+i` and `1*8+i ↔ 2*8+i`. Build `ADJ: np.uint32[24]` bitmasks.
- Mills (16 `np.uint32` masks): 12 ring mills `{r*8+i, r*8+i+1, r*8+i+2}` (mod 8) for
  each ring r and each even i ∈ {0,2,4,6}; 4 spoke mills `{0*8+i, 1*8+i, 2*8+i}` for
  odd i ∈ {1,3,5,7}. Also `POINT_MILLS[p]` = the 2 mill masks containing p.

### Symmetry

Sixteen permutations indexed by `(a, b, s)` with `a ∈ {1, -1}`, `b ∈ {0, 2, 4, 6}`,
`s ∈ {0, 1}`. Point `p = ring*8 + i` maps to `ring'*8 + i'` where
`i' = (a*i + b) mod 8` (Python's `%` already returns non-negative values) and
`ring' = 2 - ring if s else ring`. Build once:

```python
PERMS = np.zeros((16, 24), dtype=np.int64)   # PERMS[k][p] = image of point p
k = 0
for a in (1, -1):
    for b in (0, 2, 4, 6):
        for s in (0, 1):
            for ring in range(3):
                for i in range(8):
                    PERMS[k][ring*8 + i] = ((2-ring if s else ring)*8
                                            + (a*i + b) % 8)
            k += 1
```

`apply(k, mask)`: move each set bit `p` to `PERMS[k][p]`. Provide both a scalar
version and a vectorized one over an `np.uint32` array (loop over the 24 points,
OR-accumulate `((masks >> p) & 1) << PERMS[k][p]` — 24×16 cheap vector ops, fast
enough for everything in this project).

`apply_pos(k, (mover, opp))` applies the same permutation to both masks.

Canonical ordering key of a position is the tuple `(mover, opponent)` compared
lexicographically (mover first). A position is **canonical** iff no symmetry image
compares smaller; `canonicalize` returns the minimum image over all 16.

**Gate** (`tests/test_board.py`, `tests/test_symmetry.py`):
- Point degrees: exactly 12 points of degree 2, 8 of degree 3, 4 of degree 4; total
  edges = 32; every point lies in exactly 2 mills.
- Group properties: the 16 permutations are distinct; composing any two lands in the
  set; identity present (it is `(a=1, b=0, s=0)`).
- Automorphism property (this *proves* the tables are right): for every permutation,
  the image of the adjacency relation is the adjacency relation, and the set of 16
  mill masks maps onto itself.
- Canonical invariance (randomized, ≥1000 trials): for random positions `P` and
  random `k`, `canonicalize(P) == canonicalize(apply_pos(k, P))`.

## N2 — Ranking, canonical white sets, database reader (`ranking.py`, `db.py`)

### Binomials and subset rank/unrank

`BINOM[n][k]` for n ≤ 24 as int64. A `b`-element subset of an ordered "universe"
(the ascending list of the 24−w points **not** in the mover mask) is ranked by the
combinatorial number system: compact each chosen point to its index `c` within the
universe (0-based, ascending), then `rank = Σ_j C(c_j, j+1)` over the chosen compacted
indices sorted ascending `c_0 < c_1 < …`. Unrank inverts this greedily from the
largest element down: for `j = b-1 … 0`, pick the largest `c` with `C(c, j+1) ≤
remaining`, subtract. Test rank/unrank as an exact bijection over *all* subsets for
small (n ≤ 12) universes and randomized for larger ones.

### Canonical white-set tables

For each stone count `n ∈ 3..=9`: all 24-bit masks with popcount `n` that are the
minimum of their own 16-symmetry orbit (compare `apply(k, mask) >= mask` as plain
integers for all k), sorted ascending. Compute vectorized over `np.arange(2**24)`
(filter by popcount, then AND together the 16 comparisons); cache the seven arrays in
`ml/cache/canonical_sets.npz` keyed by n.

⚠ This must match the Rust tables **exactly** — the whole index scheme depends on it.

**Hard gate on sizes** (from `readme-database.md` §4.1): n=3→**158**, 4→**757**,
5→**2830**, 6→**8774**, 7→**22188**, 8→**46879**, 9→**82880**.

### Indexing

For canonical position `(mover, opp)` with counts `(w, b)`:
`white_rank = binary-search of mover in canonical_sets[w]` (must be found);
`black_rank = subset-rank of opp within the universe of points not in mover`;
`index = white_rank * C(24-w, b) + black_rank`. Subspace file size in entries:
`size(w, b) = len(canonical_sets[w]) * C(24-w, b)`.

`unindex(w, b, idx)` inverts: `white_rank, black_rank = divmod(idx, C(24-w, b))`,
mover from the table, opp by subset-unrank. ⚠ `unindex` can return a
**non-canonical** position (a "wasted slot", `readme-database.md` §4.3); that is
expected. A slot is *real* iff `unindex`'s result is its own canonical form.

### Reader (`db.py`)

`class Database(dir)`:
- Parses `manifest.json`; memmaps each `wdl_{w}_{b}.bin` as `np.memmap(dtype='<u2')`
  (⚠ explicit `<u2`, never platform-default). Checks file length == manifest `size`.
  Expose `available_pairs` so partial databases work everywhere.
- `raw(w, b, idx)` → u16 (vectorized over idx arrays).
- `decode(v)` → class + depth: `v == 0xFFFF` → DRAW; else even `v` → LOSS in `v`
  plies, odd `v` → WIN in `v` plies (side to move). Vectorized: class array in
  {-1 loss, 0 draw, +1 win} plus depth array.
- `lookup(mover, opp)` → canonicalize, index, raw, decode. ⚠ Positions where the
  opponent has < 3 stones are **not in any file**: they are an implicit LOSS in 0 for
  the side to move there. Handle in `lookup`'s caller (movegen/eval), not by probing.

**Gate** (`tests/test_ranking.py`, `tests/test_db.py`):
- Canonical-set sizes exactly match the table above.
- `size(3, 3) == 210140` (= 158 · C(21,3) = 158 · 1330) and every `size(w, b)` for
  the pairs present equals the manifest's `size` field.
- Round-trip, ≥10⁵ random slots per available subspace: whenever `unindex(idx)` is
  canonical, `index(unindex(idx)) == idx` must hold. For non-canonical (wasted)
  slots assert nothing about the stored value — it is meaningless (`0xFFFF` by
  default) — just record the measured wasted-slot fraction per subspace.
- **Tally match**: for each subspace in the partial DB, count W/L/D over *canonical
  slots only* (chunked full scan; a few seconds per small subspace) and diff against
  the saved `ninemm db-stats` output from N0. Any mismatch means your symmetry,
  tables, or ranking are wrong — stop and fix before proceeding.
  (If `db-stats` tallies turn out to be over all slots rather than canonical ones,
  its draw counts will exceed yours by exactly the wasted-slot count — reconcile with
  that in mind and document which convention matched.)

## N3 — Move generation and the forward-consistency gate (`movegen.py`, `consistency.py`)

Movement-phase legal moves from `(mover, opp)` (`readme-database.md` §6):

1. Destination sets: if mover has exactly 3 stones, each stone may **jump** to any
   empty point; otherwise each stone **slides** to an empty adjacent point (`ADJ`).
2. After moving stone `src → dst`, mill check: the move closes a mill iff one of
   `POINT_MILLS[dst]` is fully covered by the new mover mask. ⚠ Check against the
   mask *after* removing `src` and adding `dst` (a stone sliding out of and back
   into the same line still counts if the line is complete at `dst`).
3. No mill closed → one successor: `(opp, new_mover)` (⚠ perspectives swap — the
   successor is from the opponent's point of view).
4. Mill closed → one successor per legal capture: removable opponent stones are
   those **not in any complete opponent mill**, unless *all* opponent stones are in
   complete mills, in which case all are removable. Two mills closed by one move
   still capture exactly one stone. Successor: `(opp_without_captured, new_mover)`.
5. Since successors are `(new_mover = old opponent, new_opp = old mover)` and a
   capture removes a stone from the old opponent, a capture reduces the
   **successor's mover**. A successor whose mover has fewer than 3 stones is
   terminal: **LOSS in 0 for its mover** — never look it up in the database (no
   such subspace exists).
6. A side with no legal moves at its turn has lost (LOSS in 0). Only possible with
   ≥4 stones.

Represent each generated move as `(src, dst, captured_or_None, successor_position)`
— the web port in N8 mirrors this shape.

### The forward-consistency spot check — the make-or-break gate

Port of the idea in `verify.rs`/`design.md` §8.1, at spot-check scale. For ≥10⁶
random **canonical, real** slots across every available subspace, recompute the WDL
class from the successors' stored values and compare with the stored class:

- Let `S` = decoded classes of all successors (from each successor's own mover
  perspective; below-3-stone or blocked successors are LOSS in 0).
- No legal moves → stored class must be LOSS (depth 0).
- Some successor is a LOSS (for its mover) → stored class must be WIN.
- Else some successor is a DRAW → stored class must be DRAW.
- Else (all successors WIN for their movers) → stored class must be LOSS.

Optionally also check depth arithmetic on *quiet* (non-capture) successors of wins:
a stored WIN in d ≥ 3 should have a quiet successor stored LOSS in d−1. Treat depth
checks as informative (print violations); the WDL check is the hard gate.

**Gate**: zero WDL inconsistencies over ≥10⁶ sampled states on the partial DB. This
single test exercises geometry, symmetry, ranking, the reader, and movegen together
against ground truth produced by completely independent code. **Nothing downstream
is trustworthy until it passes**, and conversely once it passes at zero errors, the
Python port is almost certainly exact.

## N4 — Streaming dataset (`dataset.py`)

A torch `IterableDataset` yielding feature/label batches forever:

1. Per worker: seeded `np.random.Generator` (fold in `worker_id`).
2. Sample a batch of (subspace, idx): subspace ∝ slot count over available pairs,
   idx uniform in `[0, size)`.
3. Unindex (vectorized), drop non-canonical slots (⚠ the wasted-slot filter — see
   N2; typically a several-percent rejection, just yield a slightly smaller batch).
4. Split filter: `train`/`val`/`test` by a cheap deterministic 64-bit mix of
   `(w, b, idx)` (e.g. splitmix64), thresholded at 0.5 % / 0.5 %.
5. Labels from the memmap: class ∈ {0 loss, 1 draw, 2 win}, depth target
   `d/255` with a validity mask (decided states only).
6. Augmentation (train only): per-sample random symmetry `k ∈ [0,16)` applied to
   both masks. Labels unchanged.
7. Features, `float32[52]`: mover bits 0–23, opponent bits 24–47, counts/9,
   has-exactly-3 flags (`design-nn.md` §3).

**Gate** (`tests/test_dataset.py`): (a) label invariance — for sampled batches,
features built from all 16 symmetry images of a state get identical labels, and
`Database.lookup` on the augmented position returns the same class; (b) empirical
class frequencies over ≥10⁶ train samples match the N2 full-scan tallies per
subspace within ~1 %; (c) throughput ≥ 100k samples/s/worker (guideline — if far
below, vectorize harder before touching the trainer).

## N5 — Model and trainer (`model.py`, `train.py`)

Model per `design-nn.md` §4, exactly:

```python
class Block(nn.Module):
    def __init__(self, h):
        super().__init__()
        self.a, self.b = nn.Linear(h, h), nn.Linear(h, h)
    def forward(self, x):
        return F.relu(x + self.b(F.relu(self.a(x))))

class NmmNet(nn.Module):
    def __init__(self, h=256, blocks=4):
        super().__init__()
        self.inp = nn.Linear(52, h)
        self.body = nn.Sequential(*[Block(h) for _ in range(blocks)])
        self.wdl = nn.Linear(h, 3)
        self.depth = nn.Linear(h, 1)
    def forward(self, x):
        z = self.body(F.relu(self.inp(x)))
        return self.wdl(z), torch.sigmoid(self.depth(z)).squeeze(-1)
```

Configs: S = (128, 2), M = (256, 4) default, L = (384, 6).

Loss: `F.cross_entropy(wdl_logits, cls) + 0.25 * masked_mse(depth_pred, d/255)`
where the MSE averages only over decided states. Optimizer AdamW
(lr 3e-4, wd 1e-2), cosine decay to 1e-5 over the sample budget, batch 8192,
gradient clip 1.0. Checkpoint + metrics (per-class accuracy on a fixed val slice)
every N steps; `train.py` takes `--db`, `--config`, `--samples`, `--out` flags.

**Gate**:
1. **Memorization sanity**: train config S on *only* the {3,3} subspace (210k slots,
   can even be materialized fully) → ≥ 99.9 % WDL accuracy. If a tiny net cannot
   memorize a tiny subspace, the pipeline (not the model) is broken — most likely
   labels/features misaligned. Debug here, where iteration is seconds.
2. Train M on the full partial DB for ~2 × 10⁸ samples → val WDL accuracy ≥ 98 %
   (guideline; record the actual number as the baseline for N7).

## N6 — Evaluation suite (`evaluate.py`)

All against the exact database; runs on any (partial or full) DB. For each sampled
real position (≥10⁵, stratified per subspace):

1. Enumerate moves (N3). Ground-truth best class per `readme-database.md` §5:
   successor LOSS (min depth) ▸ successor DRAW ▸ successor WIN (max depth); a
   below-3-stones capture successor counts as LOSS in 0 (immediate win).
2. Model's choice: same rule with predicted class = argmax of successor WDL softmax
   and predicted depth, evaluated with the deployment stack under test (raw net /
   +TTA over 16 symmetries / +αβ search depth 2–4 with exact terminals at interior
   nodes and net leaves).
3. Report, per subspace and overall: **blunder rate** (model move's true class worse
   than best achievable), **optimal-move rate** (class-optimal; win-depth-optimal
   reported separately), plus state-level accuracy/confusion/depth-MAE on held-out
   states.
4. **Soak matches**: model stack vs. exact-database player (both sides alternately)
   from ≥1000 random drawn-or-better real positions, playing until capture-free-move
   or ply limits; count any model loss from a non-lost start as a failure. ⚠ The
   exact player must be driven by *stored* values (§5 rule), reusing N3 movegen —
   it is ~30 lines, not a new engine.

**Gate**: the harness runs end-to-end on the partial DB and prints the report; no
fixed numeric bar yet (that is N7's job) — but eyeball that TTA and search each
strictly reduce the blunder rate; if they don't, the stack wiring is buggy.

## N7 — Full-scale training and the scaling study

1. Produce/obtain the **full** database (`ninemm solve --dir db`, then `verify`;
   see `getting-started.md` for hardware needs). Re-run the N3 consistency gate
   against the full DB (cheap, catches nothing-or-everything).
2. Grid: {S, M, L} × sample budgets {1e8, 3e8, 1e9} (extend upward while the
   blunder-rate curve is still dropping). Single consumer GPU is ample — the model
   is loader-bound; run the loader with several workers.
3. Evaluate every run with N6 (raw / +TTA / +3-ply). Pick the smallest config
   meeting `design-nn.md` §8 targets: state accuracy ≥ 99 %, deployment-stack
   blunder rate ≤ 0.1 %/move, 0 losses in 1000 drawn-start soak games.
4. Quantization ablation on the chosen checkpoint: fp16 (expected free) and int8;
   adopt the smallest format whose N6 move-level metrics are unchanged.
5. Record everything (config, samples, seeds, metrics) in `ml/RESULTS-nn.md`.

**Gate**: a chosen checkpoint meeting (or a documented decision consciously
relaxing) the targets, with its full N6 report in `ml/RESULTS-nn.md`.

## N8 — Export and web runtime (`export.py`, `web/`)

1. `export.py`: from a checkpoint write (a) `model.onnx`; (b) `model.bin` — all
   weight matrices/vectors flattened little-endian in a fixed documented order —
   plus `model.json` (config, layer shapes/order, feature spec, value semantics);
   (c) `golden.json`: 64 random input vectors with their exact fp32 WDL logits and
   depth outputs.
2. `web/src/nn.ts`: forward pass from `model.bin` (plain `Float32Array` matmuls,
   ReLU, residual adds, softmax, sigmoid — mirror `model.py` exactly).
   **Gate**: max |logit difference| ≤ 1e-4 on `golden.json` (fp32 weights).
3. `web/src/rules.ts`: port of N3 (bit-mask positions, moves, mills, captures,
   terminals) + the 16-permutation table for TTA. **Gate**: replay a trace file of
   ≥10⁴ (position, legal-move-set, successor) triples exported from the Python
   implementation and diff exactly.
4. `web/src/engine.ts`: move choice = αβ (configurable depth 0/2/3) + root TTA +
   the §5 selection rule; below-3 captures and blocked positions resolved by rule.
5. Minimal demo page: clickable board, engine plays movement phase; opening plies
   use a trivial heuristic (prefer point maximizing own mill lines minus opponent's)
   with an honest "opening is not yet model-backed" note (`design-nn.md` §10).
   Training-tool view: per-legal-move WDL bars + predicted depth from the net.

**Gate**: the page plays complete games locally; a scripted headless match of the TS
engine vs. the Python deployment stack from 100 shared positions produces identical
move choices at search depth 0 (any diff = porting bug).

## N9 (optional, staged) — Opening-phase model

Only milestone touching Rust. Add an `export-opening` subcommand: sample placement
states (uniform over plies 1–18 game paths or canonical states), label each exactly
with `opening.rs`'s alpha-beta probing the full DB at the boundary, stream
`(white, black, w_left, b_left, value)` records to disk. Python side: extend
features with `w_left/9`, `b_left/9` (54 inputs), train a second model (or a shared
one with the two extra features zeroed for movement states — measure both), extend
the web engine to use it for plies 1–18. Gates mirror N5–N8: memorize a ply-≥16
slice first, then blunder-rate vs. exact search labels, then TS parity.

---

## Trap summary (re-read before debugging anything)

1. **`0xFFFF` means both "draw" and "never written (wasted slot)".** Every sampler,
   tally, and metric must filter slots by *canonicality of the unranked position*,
   never by stored value.
2. **Everything is side-to-move.** A successor's value is from the *opponent's*
   perspective: a successor that is a LOSS is *good* for you. If your net trains
   fine but plays terribly, an unflipped perspective in move selection is the first
   suspect.
3. **Depth parity**: even = loss, odd = win, and it is DTC within the material pair
   (`design.md` §4) — do not expect it to equal distance-to-mate, and do not do
   depth arithmetic across capture (pair-changing) moves.
4. **Below-3-stones successors are not in the database** — implicit LOSS in 0.
   Probing them will either crash (no such subspace) or silently read `(3,b)` junk
   depending on how you wrote `lookup`; handle them before lookup.
5. **`<u2` explicitly** when memmapping; the files are little-endian by spec.
6. **3 stones ⇒ jumps.** Forgetting the jump rule passes most random spot checks
   (few sampled states have exactly 3 stones and the difference only shows in
   successor sets) but fails the {3,b} consistency gates — if N3 fails only on
   subspaces with a 3-stone side, look here.
7. **Mill check uses the post-move mask** including the removal of the source point.
8. **Never compare against the database through your own code only** — the gates in
   N2/N3 diff against `ninemm db-stats` output and against stored values through an
   independent minimax recomputation precisely so that a self-consistent-but-wrong
   port cannot slip through.
