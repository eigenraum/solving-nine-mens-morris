# Getting Started

## Prerequisites

- Rust (stable toolchain via [rustup](https://rustup.rs/)). Developed against 1.97.
- A 64-bit machine. Budget **≥16 GB RAM** (the largest single pair's working set is
  ~8.5 GB; the OS needs headroom beyond that) and **~10 GB free disk** for the finished
  database.
- Multiple CPU cores strongly recommended — the solver is parallelized with `rayon` and
  scales with core count.

## Build

```sh
cargo build --release
```

The binary is `target/release/ninemm`. Everything below assumes you're running it from
the repository root; add `target/release/` to your `PATH` or invoke it directly.

## Run the tests

```sh
cargo test --release
```

This runs the fast suite (~2 minutes). Two tests are marked `#[ignore]` or otherwise
excluded from the default set because they're slow (multi-minute, since they involve
either a from-scratch oracle solve of the `{3,3}` pair or a `{4,3}`-pair oracle
cross-check):

```sh
cargo test --release -- --ignored          # Gasser paper cross-check (~16s solve + checks)
cargo test --release retro::tests::solve_4_3_matches_oracle_exactly -- --nocapture  # ~5-8 min
```

Clippy should be clean:

```sh
cargo clippy --release --all-targets
```

## Reproduce the full solve

```sh
cargo build --release
./target/release/ninemm solve --dir db
```

This walks all 28 unordered material pairs bottom-up (Gasser's Figure 4 dependency
order) and writes `db/wdl_<w>_<b>.bin` (one file per *ordered* subspace — so 49 files
total, since `{a,b}` produces both `wdl_a_b.bin` and `wdl_b_a.bin`) plus
`db/manifest.json` tracking sizes, xxh3 checksums, and solve timestamps.

- **Resumable.** Re-running skips any pair already solved on disk with a checksum
  matching the manifest. If interrupted, just run the same command again.
- **Partial runs**, useful for development/benchmarking:
  ```sh
  ./target/release/ninemm solve --dir db --max-total 10   # only pairs with a+b <= 10
  ```
- **Expect a few hours** on a modern multi-core machine (see `design.md` for the
  reasoning behind that estimate, and git history around the "M6" commits for actual
  measured timings on an 11-core/18GB test machine).

## Verify a finished (or partial) solve

```sh
./target/release/ninemm verify --dir db
```

Runs the forward-consistency scan (`design.md` §8.1) over every solved pair, stopping
at the first inconsistency (later pairs depend on earlier ones, so there's no point
continuing past a broken one). Exits non-zero if anything fails.

```sh
./target/release/ninemm db-stats --dir db
```

Prints win/loss/draw tallies and the deepest win/loss per solved subspace.

## Play against the engine

Requires the **complete** database (all 49 subspaces) — the opening search can, in
principle, reach almost any material split, so a partial database isn't enough (the
command checks for this upfront and reports a clear error rather than panicking
mid-game).

```sh
./target/release/ninemm play --dir db --human white
```

- Placement phase: enter a square, e.g. `a1`.
- Movement phase: enter `from to`, e.g. `a1 a4`.
- If your move closes a mill with more than one legal capture, you'll be prompted to
  choose which enemy stone to remove.
- Pass `--human black` to play second.

## Other useful commands

```sh
./target/release/ninemm board   # print board geometry (point numbering, adjacency, mills) — debugging aid
./target/release/ninemm stats   # print subspace sizes and canonical white-set table sizes
```

## Project layout

See the "Repository layout" section of [`README.md`](README.md).

## Troubleshooting

- **`solve` seems to be using less than all your cores**: rayon respects
  `RAYON_NUM_THREADS`; unset it (or set it explicitly) if something else in your
  environment is constraining it.
- **Out of memory during `solve`**: the largest single pair (`{9,8}`/`{8,9}`) needs
  roughly 8.5 GB of working memory in addition to its (much smaller) dependency files.
  If you're tight on RAM, there isn't currently a lower-memory mode — see
  `design.md`'s risk register for the documented tradeoff.
- **`play` panics with "no entry found for key"**: the database is incomplete for the
  position reached. This shouldn't happen if `verify --dir db` passes and all 49 files
  are present (the `play` command checks for this and should refuse to start instead —
  if you see this panic, please treat it as a bug and file an issue with the position
  that triggered it).
