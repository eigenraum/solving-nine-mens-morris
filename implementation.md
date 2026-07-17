# Solving Nine Men's Morris — Implementation Plan

Concrete build plan for the architecture in `design.md`. Single Rust crate, milestone
per section; every milestone ends with green tests and a commit. Later milestones only
depend on earlier ones, so the plan is executable top-to-bottom.

## Project layout

```
Cargo.toml
src/
  board.rs      # geometry: points, adjacency, mills, symmetry permutations
  pos.rs        # Position (u64 bitboard), phase logic, terminal detection
  movegen.rs    # forward moves (place/slide/jump/capture) + reverse quiet moves
  symmetry.rs   # canonicalization, orbit computation
  index.rs      # subspace ranking/unranking, canonical-white-set tables
  subspace.rs   # subspace metadata, file naming, manifest, DAG order
  oracle.rs     # independent naive memoized solver (verification only)
  retro.rs      # retrograde engine: init pass, frontier propagation
  opening.rs    # placement-phase alpha-beta + transposition table + ply-8 cache
  verify.rs     # forward-consistency scan, checksums
  play.rs       # perfect-play move selection
  main.rs       # CLI: solve | verify | value | play | stats | oracle-check
tests/          # integration tests
db/             # output databases (gitignored)
```

Dependencies: `rayon`, `memmap2`, `clap` (derive), `anyhow`, `xxhash-rust`,
`proptest` (dev), `criterion` (dev).

---

## M0 — Scaffolding (small)

- `cargo init`, add dependencies, CI-style `cargo fmt/clippy/test` pre-commit habit.
- `.gitignore`: `target/`, `db/`.
- CLI skeleton with `clap` subcommands stubbed.

**Done when**: `cargo run -- --help` lists subcommands.

## M1 — Board core: geometry, bitboards, move generation

1. **Point numbering**: 0–23, ring-major (outer 0–7, middle 8–15, inner 16–23,
   clockwise from a fixed corner). Document the mapping to Gasser's a1–g7 coordinates
   in comments and provide `point_name()` for I/O.
2. **Static tables** (const-built or `build.rs`/`OnceLock`):
   - `ADJ: [u32; 24]` adjacency masks (verify degree ∈ {2,3,4}, Σdeg = 2·32 edges),
   - `MILLS: [u32; 16]`, `POINT_MILLS: [[u32; 2]; 24]` (each point in exactly 2 mills).
3. **`Position`**: `u64` (white 0–23, black 24–47), always white-to-move by
   convention; `flip_colors()` swaps the halves.
4. **Forward move generation** for the movement phases, as an iterator of successor
   `Position`s (already color-flipped to opponent-to-move-normalized):
   - slide (or jump when mover has exactly 3 stones),
   - detect mill closure via `POINT_MILLS` of the destination,
   - on closure: one capture per removable stone (not-in-mill, or any if all in mills).
5. **Reverse quiet-move generation**: predecessors within the pair — stone moves
   *from* an empty adjacent point (jump-from-anywhere when mover has 3), and the moved
   stone must **not** have closed a mill at its origin-successor… careful: a reverse
   move is valid iff the *forward* move it mirrors was quiet, i.e. the moving stone's
   destination (the current point) did *not* complete a mill. Assert this by
   round-tripping in tests.
6. **Terminal checks**: `is_blocked()`, stone counts.

**Tests**:
- Table sanity (degrees, mill membership, edge count, Gasser Fig. 1–3 positions).
- Round-trip: for random positions, every reverse-move's forward-move set contains
  the original (proptest).
- Known move counts on hand-constructed positions (blocked position of Fig. 3).

## M2 — Symmetry and canonicalization

1. The 16 permutations: generate from two generators (rotation 90°, mirror) × ring
   swap (outer↔inner); store as `[[u8; 24]; 16]`.
2. Apply a permutation to a 24-bit mask via precomputed 4×6-bit chunk lookup tables
   (or a plain 24-step gather first; optimize later — benchmark before optimizing).
3. `canonicalize(pos) -> (Position, sym_id)`: min over 16 of `(perm(white), perm(black))`
   as a 48-bit key.

**Tests**:
- Group closure: composing any two of the 16 permutations lands in the set; identity
  present; each has an inverse.
- Automorphism property: every permutation maps `ADJ` onto `ADJ` and `MILLS` onto
  `MILLS` (this *proves* the symmetry set is valid).
- Canonical form is invariant under randomly applied symmetries (proptest).

## M3 — Indexing

1. Binomial table `C[n][k]` for n ≤ 24; subset rank/unrank (combinatorial number
   system) over a masked universe (black ranks within the 24−w non-white points).
2. Canonical-white-set tables per w ∈ [3,9]: enumerate C(24,w) subsets, keep orbit
   minima, sort → `Vec<u32>` + reverse lookup (binary search or hashmap). Also store
   each canonical set's stabilizer subgroup (needed to pick the canonical black set
   deterministically and, later, for exact stats).
3. `index(pos) -> (SubspaceId, u64)` and `unindex(subspace, i) -> Position`
   (unindex may return any orbit representative; it must round-trip through `index`).
4. Subspace metadata: `size(w,b) = n_canonical_white(w) * C(24-w, b)`; DAG order list
   of the 28 unordered pairs.

**Tests**:
- rank/unrank bijection per subset size (exhaustive for small, proptest for large).
- `index(unindex(i)) == i` for random i in every subspace.
- `index` is symmetry-invariant: `index(pos) == index(random_sym(pos))` (proptest).
- Total sizes match the computed table (Σ ≈ 8.9×10⁹ slots; print per-subspace sizes
  and compare against the design doc's numbers).

## M4 — Independent oracle (verification baseline)

Naive solver, deliberately sharing *only* `movegen` with the main path (no symmetry,
no indexing): hashmap-memoized depth-first minimax with cycle handling via the
standard "draw upon repeat within stack; finalize on unwind" retrograde-free approach —
simplest correct: run **value iteration** over an explicit hashmap graph of the full
`{3,3}` pair (2.7 M raw states) until fixpoint, deriving win/loss depths.

**Tests**: oracle self-consistency (minimax check over its own results); Gasser Fig. 9
(3-3 position, loss in 26 plies) must reproduce.

## M5 — Retrograde engine, single pair, single-threaded

1. `Val`/`Count` byte union codec exactly as design §4 (encode/decode helpers, tested).
2. Init scan over both ordered subspaces of the pair: terminal losses, win-in-1 to
   2 stones, capture-successor probing into (mmapped) solved pairs, Count setup.
3. Frontier propagation with two alternating queues (depth levels), reverse-move
   predecessor generation, canonicalize→index→update.
4. Solve `{3,3}` (no capture dependencies — self-contained) and **diff every state
   value and depth against the oracle (M4)**. This is the make-or-break correctness
   gate for the whole project; do not proceed until it's exact.
5. Solve `{4,3}` (depends on `{3,3}`) and spot-check ~10⁵ random states against the
   oracle extended with database probing.

**Tests**: the above diffs, plus determinism (two runs → identical bytes).

## M6 — Parallelism + full-scale orchestration

1. Parallelize init scan (rayon over index ranges) and frontier levels (per-thread
   buffers, `AtomicU8` for Count decrements, dedup on frontier merge). Re-verify
   `{3,3}`/`{4,3}` byte-identical with the single-threaded result.
2. `solve` CLI command: walks the 28-pair DAG, skips pairs whose files+checksums
   exist, writes `db/wdl_w_b.bin` + manifest entry after each pair (resumable).
3. Memory strategy: current pair's arrays in RAM; dependency files mmapped read-only.
   Track RSS; worst pair {9,8}+{8,9} ≈ 2.1 GB live.
4. Benchmarks on `{6,5}`-sized pairs → projected full-run time; tune (chunk sizes,
   prefetch, canonicalization fast path) only if projection exceeds ~12 h.
5. **Full run** for all 28 pairs. Log per-pair timings, state counts,
   win/loss/draw tallies.

**Done when**: `db/` holds all 49 subspace files with manifest + checksums.

## M7 — Verification suite

1. `verify` command: parallel forward-consistency scan of every state (recompute
   minimax from stored successor values; check WDL and depth arithmetic), per design
   §8.1. Run over the entire database.
2. Statistics report (`stats` command): per-subspace win/draw/loss percentages →
   compare against Gasser Fig. 11's shape (e.g. 3-3 ≈ 83 % wins for side to move);
   longest win depths (expect a 187-ply mill-closure win in {8,7}).
3. Re-run one large pair from scratch → byte-identical (guards against races).

**Done when**: full-database scan reports zero inconsistencies and stats match
published reference points.

## M8 — Opening search

1. Placement-phase state `(white, black, w_left, b_left)`; movegen for placements
   incl. captures; terminal/DB probe at the phase boundary (also mid-opening
   terminals: a side dropping below 3 during placement, blocked positions).
2. Canonicalizing transposition table (hashmap, 64-bit keys of canonical state +
   hand counts); alpha-beta with capture-first move ordering.
3. `value` command on the empty board → **expected output: draw**. This reproduces
   Gasser's headline result with exact values rather than bounds.
4. Persist all ply-≤8 canonical evaluations to `db/opening_cache.bin`.
5. Cross-check Gasser Fig. 12 (black to move, all moves draw), Fig. 13/14 (White's
   losing 3rd move refuting the mill-rush strategy).

## M9 — Perfect player + polish

1. `play` command: interactive CLI (text board, a1–g7 move notation), engine follows
   design §7 move selection (min-depth wins / draws / max-depth losses); opening moves
   answered from the ply-8 cache + shallow search.
2. Self-play soak test with the win-depth-progress invariant checker (design §8.5).
3. Optional 2-bit WDL export (`export --wdl2`) for a ~2.3 GB distributable database.
4. README: results summary, how to reproduce, timings, database format spec.

## Risk register (checked continuously)

| Risk | Mitigation |
|---|---|
| Depth overflow of the byte codec (win/loss depths ≥ ~250 within a pair) | DTC-style per-pair depths keep the known max ≈ 187; add a saturation assert in the codec — if it ever fires, widen to a side-array for that pair |
| Subtle reverse-move bugs (the classic retrograde failure mode) | M1 proptest round-trips + M5 oracle diff gate on `{3,3}`/`{4,3}` before scaling |
| Race conditions in parallel propagation | byte-identical check vs. single-threaded run (M6.1); deterministic level ordering |
| Index slots for stabilizer-duplicate states drift out of sync | treat duplicates as ordinary states (they're solved redundantly and consistently); verify scan covers them like any state |
| RAM pressure on 16 GB machines | per-pair residency + mmapped dependencies; document 32 GB as comfortable target |
| Wrong rule interpretation vs. Gasser | rule decisions pinned in design §1; cross-checks against his published positions in M4/M7/M8 |

## Estimated effort

Rough compute budget: full solve a few hours on 8–16 cores, verification scan similar.
Coding-wise, M1–M5 are the correctness-critical core; M6 is engineering; M7–M9 are
mostly mechanical once M5's oracle gate passes.
