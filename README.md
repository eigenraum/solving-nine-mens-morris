# Solving Nine Men's Morris

A from-scratch, independently-verified solver for Nine Men's Morris, following the
two-phase approach of Ralph Gasser's 1996 paper *Solving Nine Men's Morris* (retrograde
analysis for the mid/endgame, alpha-beta search for the opening), modernized for
current hardware: the entire ~9-billion-state mid/endgame database fits in RAM, so
every value is computed exactly rather than as a bound, and the whole thing runs in
hours on a laptop instead of years across a cluster of 1990s machines.

See [`design.md`](design.md) for the full architecture rationale and
[`implementation.md`](implementation.md) for the milestone-by-milestone build plan this
was implemented against. For details on *using* the generated database from another
program, see [`readme-database.md`](readme-database.md). To get building and running
yourself, see [`getting-started.md`](getting-started.md).

## Result

Nine Men's Morris is a **draw** with perfect play — matching Gasser's original result,
reproduced here independently. See [`RESULTS.md`](RESULTS.md) for the verified
statistics (win/loss/draw tallies and maximum depths per subspace) once the full solve
has completed and been checksummed.

## How this differs from Gasser's approach

- **Language**: Rust, not C/Pascal-of-the-era. Chosen for safe fearless concurrency
  (the retrograde solver is heavily multithreaded via `rayon`), zero-cost abstractions,
  and a strong testing story.
- **No byte-packed Val/Count union.** Gasser packed win/loss depth and a live successor
  count into a single byte because his machines had tens of megabytes of RAM. We use a
  `u16` per state plus separate scratch arrays during solving — simpler, more robust
  (his scheme only works because max depth + max branching factor stays under 255;
  that's a real fragility we don't need to accept), and affordable given the RAM budget
  on any current machine.
- **Exact values everywhere**, not bounds. Gasser's opening search worked from
  compressed 1-bit-per-state bound databases because his full 9 GB database didn't fit
  in 72 MB of RAM. Ours does fit in RAM (or is close enough that mmap'd disk access is
  effectively free), so the opening search queries exact values directly.
- **An analytical symmetry-orbit formula** replaces a per-state simulation step in the
  retrograde solver's initialization, giving a 5.5–9× speedup on the pairs it affected
  most (see the M6 commits in git history for the full derivation and how it was
  validated).

## Verification

Results are checked two independent ways, per [`design.md`](design.md) §8:

1. **An independently-written oracle** (`src/oracle.rs`) — plain forward value
   iteration over raw, unreduced states, sharing only basic move-generation rules with
   the retrograde solver. Full agreement (every value, every depth) on the `{3,3}` and
   `{4,3}` pairs was the gate for trusting the retrograde engine at all; this process
   found and fixed several real bugs before the full solve was ever attempted.
2. **A forward-consistency scan** (`src/verify.rs`) — for every state in the finished
   database, recompute its value directly from its successors' *stored* values (a
   single-pass minimax check, algorithmically unrelated to the solver's own
   bucket-queue propagation) and confirm it matches what's on disk.

Both cross-checks additionally reproduce specific numbers published in Gasser's paper:
a longest loss of exactly 26 plies in the `{3,3}` subspace (his Figure 9), and a ~83%
win rate for the side to move in `{3,3}` (his Figure 11).

## Repository layout

```
src/
  board.rs       geometry: 24 points, adjacency, 16 mills
  pos.rs         Position bitboard type (always "side-to-move" normalized)
  movegen.rs     forward moves + reverse quiet-move generation
  symmetry.rs    16-element board automorphism group, canonicalization
  index.rs       near-perfect hash: position -> (subspace, dense index)
  oracle.rs      independent verification oracle (naive, from scratch)
  retro.rs       the retrograde solver (parallel, atomics-based)
  persist.rs     on-disk database format + manifest
  orchestrate.rs walks the 28-pair solve DAG, resumable
  verify.rs      forward-consistency verification suite
  opening.rs     18-ply placement-phase alpha-beta search
  play.rs        perfect-play move selection + self-play soak test
  server.rs      HTTP server for the browser UI: JSON analysis API over the database
  main.rs        CLI: board | stats | solve | verify | db-stats | play | serve
ui/index.html         the one browser UI: board, play, evaluation overlay; drives either
                      engine backend (exact database via /api, or the neural net in-browser)
ml/                   Python: dataset, training, and export of the compressing value network
web/                  TypeScript: in-browser rules + NN engine; provider.ts adapts it to the
                      same analysis contract as server.rs, so ui/index.html can use both
design.md            architecture and rationale
implementation.md    milestone-by-milestone build plan
getting-started.md   build, run, reproduce
readme-database.md   on-disk format spec, for external tools consuming the database
readme-agent.md      orientation for AI agents working in this repo
design-nn.md          design: lightweight NN compressing the database for in-browser play
implementation-nn.md  milestone build plan for the NN (data pipeline, training, web export)
ui-design.md          browser UI: architecture and rationale
ui-implementation.md  browser UI: milestone-by-milestone build plan
GasserArticle.pdf    the paper this project follows
```
