# Agent Notes

Orientation for an AI agent (or any future contributor) picking up work in this repo.
Read `design.md` first for the "why", `implementation.md` for the milestone plan this
was built against, then come back here for the "what actually happened, what to watch
out for, and where the bodies are buried."

## Project state

Solving Nine Men's Morris completely: retrograde analysis for the mid/endgame
(3–9 stones per side), alpha-beta for the 18-ply opening. As of the last update to this
file, milestones M0–M9 (see `implementation.md`) are implemented and each individually
verified; the full 28-pair background solve was in progress. Check `db/manifest.json`
— if it lists all 49 `(w,b)` entries for `w,b` in `3..=9`, the solve is complete; run
`./target/release/ninemm verify --dir db` to confirm integrity before trusting it, and
check `RESULTS.md` (if present) for the final headline numbers.

## Module map (dependency order, roughly bottom-up)

| Module | Role | Depends on |
|---|---|---|
| `board.rs` | Point numbering, adjacency, mills (all `const fn`, computed at compile time) | — |
| `pos.rs` | `Position` bitboard: always `(mover, opponent)`, never physical color | `board` |
| `movegen.rs` | Forward moves (placement/movement + captures) and **reverse** quiet-move generation | `board`, `pos` |
| `symmetry.rs` | 16-element board automorphism group, canonicalization, `stabilizer_size` | `pos` |
| `index.rs` | Position → `(SubspaceId, u64)` near-perfect hash | `pos`, `symmetry` |
| `oracle.rs` | Independent verification oracle (naive Bellman-Ford, shares only `movegen`) | `movegen`, `pos` |
| `retro.rs` | The retrograde solver — parallel, atomics-based | `index`, `movegen`, `pos` |
| `persist.rs` | On-disk format: one file per ordered subspace + JSON manifest | — |
| `orchestrate.rs` | Walks the 28-pair DAG, resumable | `index`, `persist`, `retro` |
| `verify.rs` | Forward-consistency scan, tallies | `index`, `movegen`, `persist`, `retro` |
| `opening.rs` | 18-ply placement-phase alpha-beta + TT | `movegen`, `pos`, `retro`, `symmetry` |
| `play.rs` | Perfect-play move selection, self-play soak test | `board`, `movegen`, `opening`, `pos`, `retro` |
| `main.rs` | CLI glue | everything |

## The single most important invariant

**Every `Position` is normalized to "mover to move."** `pos.white()` is *always* the
side-to-move's stones, `pos.black()` is *always* the opponent's — regardless of actual
stone color. This is why `retro.rs`'s `Side` struct for pair `{a,b}` needs *two* arrays
(subspace `(a,b)` and `(b,a)`) — a quiet move flips whose stones are "mover," so it
lands in the *other* ordered subspace of the same unordered pair. Get this backwards
and everything downstream is subtly wrong in a way that's easy to not notice until an
oracle cross-check catches it (this happened — twice — see "Bugs found" below).

## Bugs found during development (read before touching `retro.rs`)

This area of the code has hidden three real, independent bugs, each only caught by the
oracle cross-check or a debug-mode assertion — not by casual code review, not by the
parallel-vs-sequential determinism check, not by clippy. If you're modifying
`retro.rs`, **re-run the oracle cross-check** (`solve_3_3_matches_oracle_exactly` and
especially `solve_4_3_matches_oracle_exactly`, the latter because it's the smallest
pair that exercises `a != b` and cross-pair capture lookups) before trusting any
change, no matter how obviously-correct it looks.

1. **Count multiplicity vs. symmetry stabilizers.** `init_side`'s `count` field must
   equal the number of decrement *events* a state will receive during propagation, not
   its raw quiet-successor count. When a state's own symmetry stabilizer is
   nontrivial, several raw successors collapse into one canonical class that only
   decides once — so raw-successor-count overcounts. Current code computes this via an
   analytical orbit-counting formula (`stab(C) * f(P,C) / stab(P)`, see the long
   comment in `init_side`), which itself replaced an earlier direct-simulation
   approach after a benchmarking pass showed the simulation was the bottleneck on
   jump-heavy pairs. **There is a permanent debug-only cross-check** between the
   formula and the simulation (`#[cfg(debug_assertions)]` block right after the
   formula) — if you ever suspect this area, run the test suite in a *debug* build
   (`cargo test`, not `cargo test --release`) first; it's slow but will catch a
   regression here immediately, before you burn hours on a release-mode oracle run.

2. **Premature commit of capture-based wins.** A state whose only known-good option
   at init time is a capture into an already-solved smaller pair must **not** be
   written to `val[]` immediately — it might have a quiet successor that resolves
   (via propagation, later) to an even better win, and propagation's own "already
   decided" guard would then incorrectly treat the state as settled. Fixed by
   deferring every win/loss decision through the bucket-queue's `should_process`
   commit-on-pop mechanism uniformly, so a smaller-depth win discovered in an earlier
   bucket always wins the race. If you ever see a stored depth that's *larger* than an
   oracle-computed minimum (not smaller — smaller-than-oracle would point elsewhere),
   suspect this class of bug first.

3. **Uninitialized `count` for capture-win states.** A state that returns early via a
   capture-based tentative win never initializes `count`/`max_seen_depth` (irrelevant
   to *its own* decision), but another state's propagation can still target it (a
   decrement from an unrelated quiet successor). Left at the default `0`, that
   decrement underflows — invisible in release builds (wraps to a value no realistic
   decrement count could ever bring back to exactly zero) but trips `debug_assert!` in
   debug builds. Fixed by sentinel-initializing to `u32::MAX`. **This is why the debug
   build matters**: this bug shipped, passed every release-mode oracle test, and was
   only found because a debug-mode test run was used to validate an unrelated
   optimization. If you add new early-return paths in `init_side`, ask whether they
   leave `count`/`max_seen_depth` at a safe value for a *different* state's
   propagation to target later.

**General lesson**: this codebase treats "matches an independent oracle exactly" as the
bar for correctness, not "looks right" or "passes the tests I wrote alongside the
change." Every one of the three bugs above passed hand-written unit tests. Only the
oracle (an intentionally different algorithm/code path) or a debug-mode assertion
caught them. When touching `retro.rs`, budget for an oracle re-run — the `{4,3}` one
takes 5–8 minutes even on a fast machine (the oracle side, not our solver, is the slow
part; it's a deliberately naive, unoptimized reference implementation).

## Testing tiers

```sh
cargo test --release                          # fast suite, ~2 min
cargo test --release -- --ignored              # + the ~16s Gasser-paper cross-check
cargo test --release retro::tests::solve_4_3_matches_oracle_exactly -- --nocapture  # ~5-8 min, the strongest gate
cargo test                                     # debug build — slow, but activates the retro.rs reciprocal-count cross-check
```

If you're making a change to `retro.rs` or `index.rs`'s canonicalization/indexing
logic, run all four, in roughly that order (fail fast on the cheap ones first).

## Concurrency model (retro.rs)

`Side`'s three per-state arrays use atomics (`AtomicU16`/`AtomicU32`) with `SeqCst`
throughout — not because weaker orderings are provably unsafe here, but because
reasoning precisely about which weaker orderings *are* safe for this particular
multi-variable protocol (a `count` reaching zero must observe every concurrent
`max_seen_depth` update that happened first) was judged not worth the risk on a
computation spanning billions of states. If you're tempted to relax this for
performance, benchmark first — `retro.rs`'s init phase and propagation are usually not
the bottleneck compared to I/O and the sheer state count, and a silent ordering bug
here is much more expensive than the fixed per-operation cost of `SeqCst`.

## Performance notes

- The `{3,x}` pairs (any pair with a 3-stone, jump-capable side) are disproportionately
  expensive relative to their state count, because jump moves have much higher
  branching factor than slides. The `stabilizer_size`-based analytical reciprocal-count
  formula (see bug #1 above) was specifically the fix for this; before it, these pairs
  were 7–10× slower than they needed to be.
- Each pair `{a,b}` depends on at most **two** smaller pairs — `{a,b-1}` and
  `{a-1,b}` (Gasser's Figure 4 DAG) — never the full set of previously-solved pairs.
  `orchestrate.rs` relies on this to keep memory bounded regardless of how far through
  the 28-pair DAG a run has progressed; don't accidentally load more than that when
  extending it (e.g. for a future full-opening-database feature).
- `persist.rs`'s file I/O uses raw pointer casts (`u16` slice ↔ `u8` slice) for speed
  over hundreds of millions of elements — native-endian, not portable across
  differently-endian platforms. This is a deliberate, documented tradeoff (see the
  doc comment on `as_bytes`), not an oversight.

## Things that are *not* implemented (see design.md §9 / implementation.md)

- **Full opening database.** Only the empty-board value is proven via search; a
  complete ply-by-ply opening database (~2.7×10¹⁰ states after symmetry) was scoped as
  optional in the design and not built.
- **Ultra-strong play** (preferring moves that maximize opponent error chances among
  equal-value options) — not implemented; `play.rs`'s move selection picks *a* value-
  optimal move, not necessarily the practically-hardest one.
- **2-bit WDL export** for a smaller distributable database — not implemented; the
  on-disk format is the full `u16`-per-state array (see `readme-database.md`).

## If you're resuming an interrupted background solve

`orchestrate::solve_all` is resumable by construction (checks the manifest + file
checksum before re-solving any pair). Just re-run
`./target/release/ninemm solve --dir db`. If the process died mid-write, `persist.rs`
writes to a `.tmp` file and renames atomically, so a partially-written file should never
be mistaken for a finished one — but it's cheap to double check with
`./target/release/ninemm verify --dir db` once it finishes, regardless.

## Where the historical detail lives

Each milestone's commit message documents what was built and, where relevant, what bug
was found and how it was diagnosed — these are worth reading via `git log` if you need
the full reasoning behind a piece of code, not just the summary above.
