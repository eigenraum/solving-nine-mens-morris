# Persistent Opening-Phase Cache — Design & Implementation Plan

**Status**: not started. This document is self-contained: read this plus the two files
it points to (`src/opening.rs`, `src/persist.rs`) and you should have everything needed
to implement it without further context from whoever wrote this plan.

## Background

Nine Men's Morris splits into two phases with different solving strategies (see
`design.md` §3, §6 for the full rationale):

- **Movement/endgame** (3–9 stones per side, both placed): solved exhaustively by
  retrograde analysis, persisted to disk as `db/wdl_<w>_<b>.bin` (`src/retro.rs`,
  `src/persist.rs`). This part is complete, exhaustively verified, and out of scope for
  this plan.
- **Opening/placement** (18 plies, stones being placed): acyclic, so it's solved by
  plain alpha-beta negamax with an in-memory transposition table (`src/opening.rs`),
  probing the movement-phase database at the placement→movement boundary. Only ever
  run **in-memory**; nothing about it is persisted to disk.

`design.md` §6 always intended a middle ground here: a small on-disk cache of shallow
opening positions, so repeated searches (every fresh `play`/`serve`/`solve-opening`
invocation currently starts from an empty transposition table) don't redo the same
work. `implementation.md`'s M8 milestone explicitly named this
(`db/opening_cache.bin`, "ply-≤8 canonical evaluations") but it was never built. This
plan is that missing piece.

**Why it matters**: every consumer of `opening::solve`/`opening::solve_from_empty_board`
currently starts with a fresh, empty `HashMap` (see exact call sites below) and
re-explores the same shallow part of the 18-ply tree from scratch every time. The
shallow levels are the ones most likely to be revisited across different games/lines
(branching diversity is much lower near the root than deep in the tree), so caching
them is where the leverage is — exactly Gasser's own reasoning for his equivalent
"intermediate database" (`GasserArticle.pdf`, §5: "all positions that were visited by
the alpha-beta searches at the 8-ply level were stored in an intermediate database...
allows games to be played in real time").

### Exact current state (read these before starting)

- `src/opening.rs`:
  - `PlacementState { pos: Position, mover_hand: u8, opp_hand: u8 }` — the search node.
  - `Value` — 3-valued outcome (`Loss`/`Draw`/`Win`), no depth tracking. (See "Related
    but out-of-scope gap" below.)
  - `tt_key(state) -> (Position, u8, u8)` — canonicalizes `state.pos` via
    `symmetry::canonicalize`, keeps hand counts as-is (they're already
    symmetry-invariant). **This is the exact key you must match** in the persistent
    cache format, or cache hit rates will be wrong.
  - `negamax(state, alpha, beta, db, tt: &mut HashMap<(Position, u8, u8), i8>) -> i8` —
    `pub(crate)`, the actual search. Checks `tt.get(&key)` before recursing, inserts
    `tt.insert(key, best)` after computing. This is the *only* place cache reads/writes
    happen; the persistent cache's job is to pre-populate this same `HashMap` before a
    search starts (see "Load" below) and to harvest entries from it afterward (see
    "Build" below). You should not need to touch `negamax`'s logic at all.
  - `solve`/`solve_from_empty_board` — public entry points, each currently constructs
    a fresh `HashMap::new()`.
- Three call sites currently build a TT from scratch, all of which should switch to
  loading the persistent cache instead:
  - `src/main.rs` (`run_play`, ~line 281): `let mut tt: HashMap<(Position, u8, u8), i8> = HashMap::new();` — used for one interactive `play` session.
  - `src/main.rs` (`Commands::SolveOpening` handler, ~line 165): calls
    `opening::solve_from_empty_board(&db)`, which internally makes its own empty TT.
  - `src/server.rs`: `pub type Tt = HashMap<(Position, u8, u8), i8>;` (line 32) — a
    **long-lived, shared, in-process** TT that already persists across HTTP requests
    for the lifetime of the server (see the doc comment right above it), populated
    lazily and via an optional `--warm` startup pass (~line 102,
    `opening::solve(&PlacementState::initial(), &loaded.db, &mut tt)`). This already
    solves the "repeated requests within one server run" problem — what it doesn't
    solve is "every fresh server *start* pays the cost again." This is your primary
    target for the biggest practical win.
- `src/persist.rs` has the conventions to follow for the new file format: a manifest
  entry pattern (`ManifestEntry`/`Manifest` — you likely want a *separate* small
  manifest or header for this cache, not entries jammed into the movement-phase
  `Manifest`), atomic writes (`write_subspace`'s temp-file-then-rename pattern), xxh3
  checksums (`xxhash_rust::xxh3::xxh3_64`), and the existing `mmap_subspace`/
  `mmap_get_u16` pattern for read-only memory-mapped access — **use mmap for reading
  this cache too**, for the same reason the movement-phase database does (see the OOM
  history in git log around "insert_mmap" if you want the full cautionary tale: loading
  a large read-only structure fully into owned memory on a RAM-constrained machine is a
  real way to crash the process).

### Related but out-of-scope gap (flag, don't fix, unless trivial)

`opening.rs`'s `Value` is 3-valued (Win/Draw/Loss) with **no depth tracking**, so
`play::best_placement_move` (src/play.rs) cannot currently prefer a faster win or a
slower loss the way `play::best_movement_move` does for the movement phase (via the
depth-coded `u16` values in the retrograde database — see design.md §7's move-selection
rule: "win with minimal depth ▸ draw ▸ loss with maximal depth"). If you find this
trivial to fix while you're already in this code (e.g., swap `i8` for a depth-coded
`i16` throughout negamax, mirroring `retro.rs`'s even/odd depth-parity encoding), it's
a reasonable bonus — but it is not required for this plan, and the cache format
described below should work either way (see "Future extensibility" in the format spec).
Don't let it block or complicate the core task.

## Goals

1. A `build-opening-cache` step that runs (or reuses) an opening search and persists
   its shallow (`mover_hand + opp_hand >= threshold`, i.e. ply ≤ `18 - threshold`,
   default threshold matching Gasser's ply-8 cutoff) transposition-table entries to
   disk.
2. Every consumer of the opening search (`play`, `serve`, `solve-opening`) transparently
   loads this cache (if present and valid) to pre-populate its TT before searching,
   with **zero change in search results** — only speed.
3. Staleness safety: if the movement-phase database has changed since the cache was
   built (e.g., after a re-solve), the cache must be detected as stale and ignored, not
   silently served as if still valid. This class of bug — serving results computed
   against old data after the underlying data changed — is exactly what caused real
   problems earlier in this project's history (see the "double-processing bug" and
   the subsequent full re-verify in git log); treat this requirement as seriously as
   that one.
4. Memory-safe at the scale of the full database: read via mmap, not full-load.

## Non-goals (explicitly out of scope for this task)

- Making the cache grow organically across sessions (writing back newly-computed
  entries after each `play`/`serve` run). One explicit build step, read-only
  consumption thereafter. Note it as a possible future enhancement in your PR
  description if you want, but don't build it now — it adds concurrent-write
  complexity for uncertain benefit.
- Depth-aware `Value` (see "Related but out-of-scope gap" above).
- A full opening database (`design.md` §9's optional extension, ~2.7×10¹⁰ states,
  ~27 GB) — this plan is specifically the *shallow cache*, not that.
- Solving the underlying "full search is slow on a RAM-constrained machine because the
  17 GB movement-phase database barely fits" problem — that's a separate concern
  (`RESULTS.md`'s "Known limitation" section discusses it; a cache-locality-aware move
  ordering in `opening.rs` would be the fix, not this cache). This cache *helps*
  amortize that cost across repeated runs, but doesn't fix any single run's cost.

## Design

### Cache key and value

Reuse `opening::tt_key`'s exact scheme: `(canonical Position, mover_hand: u8,
opp_hand: u8)`.

Pack into a `u64` for compact on-disk storage:

```
key = (white as u64)                    // bits 0..24
    | (black as u64) << 24              // bits 24..48
    | (mover_hand as u64) << 48         // bits 48..52 (4 bits, 0..=9 fits)
    | (opp_hand as u64) << 52           // bits 52..56 (4 bits, 0..=9 fits)
```

(56 bits used, 8 spare at the top — leave them zero for now; a future depth-aware
version could use them, see "Future extensibility" below.)

Value: 1 byte. Matching `negamax`'s internal `i8` representation directly is simplest:
store the raw `i8` (`-1`/`0`/`1`) as its `u8` bit pattern (`value as u8`), decode with
`byte as i8`. Document this clearly since it's a slightly unusual choice (most of this
codebase's on-disk formats use an unsigned sentinel scheme like `retro::DRAW =
u16::MAX` — either is fine here since the domain is only 3 values, but be consistent
and explicit about which you picked).

### File format

One file, e.g. `db/opening_cache.bin`, plus a small header (don't reuse
`persist::Manifest` — that's specifically the movement-phase per-subspace manifest;
this is a single self-describing file, simpler to give it its own header):

```
[8 bytes]  magic + version, e.g. b"NMMOPEN1"
[8 bytes]  database fingerprint (u64 xxh3, see "Staleness" below), little/native-endian
[8 bytes]  entry count (u64)
[entries]  sorted ascending by key, 9 bytes each: [8 bytes key][1 byte value]
```

Sorted by key enables binary search for lookups without loading the whole file (via
mmap + `binary_search_by_key` over a slice view — see implementation notes below for
how to interpret the mmap'd bytes as a `&[Entry]`-like view safely, or just do manual
byte-offset binary search similar to `persist::mmap_get_u16`'s style).

Use `persist::write_subspace`'s pattern for the write path: build the full byte buffer
in memory (this file is small — see size estimate below — so this is fine, unlike the
multi-GB movement-phase files), write to a `.tmp` path, `rename` into place atomically.

**Size estimate**: Gasser's own root-proving search visited ~19,906 nodes at his 8-ply
boundary (`GasserArticle.pdf` §5). Our search will visit a different but comparable
order of magnitude — expect low-hundreds-of-thousands of cached entries at most for a
ply-8 cutoff, i.e. a few MB. Confirm this empirically once you have a working build
step; if it's dramatically larger than expected (e.g., tens of millions of entries),
that's worth investigating before proceeding (could indicate a key-collision or
canonicalization bug making the cache far less selective than it should be).

### Staleness: the database fingerprint

Compute a single `u64` fingerprint over the *current* movement-phase database's
manifest: e.g. `xxh3_64` over the concatenation of `"{w}-{b}:{xxh3}"` for every entry
in `persist::Manifest`, sorted by `(w, b)` for determinism. Store this in the cache
file's header at build time. On load, recompute the current database's fingerprint the
same way and compare:

- **Match** → trust the cache, load it.
- **Mismatch** → the movement-phase database has changed since this cache was built.
  Log a clear message (e.g. `"opening cache at {path} is stale (database has changed
  since it was built); ignoring"`) and proceed with an empty TT, exactly as if no cache
  file existed. Do not error out — a stale cache should degrade to "no cache," not "the
  command doesn't work."
- **File missing / corrupt / wrong magic-version** → same treatment: log once, proceed
  uncached. Never let a bad cache file crash `play`/`serve`/`solve-opening`.

### Build

A new CLI subcommand, e.g. `ninemm build-opening-cache --dir db [--max-ply 8]`:

1. Load the movement-phase database (mmap, same pattern as `Commands::SolveOpening`).
2. Run `opening::solve_from_empty_board`-equivalent logic — either call it directly if
   its signature is convenient, or (more likely useful) refactor slightly so you can
   pass in a fresh `HashMap` and get it back afterward (right now
   `solve_from_empty_board` owns its TT internally and only returns the `Value` —
   you'll want the populated TT too. Adding a variant like `solve_from_empty_board_with_tt(db: &Database) -> (Value, HashMap<(Position,u8,u8), i8>)`, or just inlining
   `solve(&PlacementState::initial(), db, &mut tt)` at the call site with a TT you keep,
   is a small, low-risk change to `opening.rs`).
3. Filter the resulting TT to entries where `mover_hand + opp_hand >= 18 - max_ply`
   (default `max_ply = 8`, so `>= 10`).
4. Compute the current database fingerprint (see above).
5. Write the sorted binary file as specified, atomically.
6. Print a summary: entry count, file size, build wall-clock time.

### Load

A small helper, e.g. `persist::load_opening_cache(dir: &Path, manifest: &Manifest) ->
Option<memmap2::Mmap>` (returns `None` and logs on any staleness/missing/corrupt
condition per "Staleness" above — or split into a "check fingerprint" step plus a
plain mmap, whichever composes better with your error handling), plus a function to
populate a `HashMap<(Position, u8, u8), i8>` from the mapped bytes (iterate all
entries — at the expected size of a few MB / low hundreds of thousands of entries,
just loading all of them into the `HashMap` up front is simpler and fine; you don't
need lazy/binary-search access for *this* consumption pattern, only the file format
itself needs to support binary search as a nice-to-have for potential future use
cases like a debug "look up this specific position" command).

Wire this into all three consumers:

- `main.rs`'s `run_play`: replace `HashMap::new()` with load-cache-then-fall-back-to-empty.
- `main.rs`'s `Commands::SolveOpening` handler: same — this one's slightly interesting
  since it's *also* a natural place to ask "did loading the cache measurably help?" by
  timing with/without it, worth logging.
- `server.rs`: same, for both the `--warm` startup path and the lazily-initialized `Tt`
  used per-request.

## Implementation plan

1. **File format module.** Add cache read/write functions to `persist.rs` (or a new
   `opening_cache.rs` module if you prefer keeping `persist.rs` focused on the
   movement-phase format — your call, but keep the atomic-write and mmap-read
   *patterns* consistent with the existing code either way). Unit tests: write then
   read back a small synthetic cache, confirm round-trip; corrupt a byte and confirm
   graceful fallback (not a panic); build two caches with different fingerprints and
   confirm a mismatched one is correctly rejected.
2. **Refactor `opening.rs`** minimally to expose the populated TT after a root solve
   (see "Build" step 2 above). Keep `solve`/`solve_from_empty_board`'s existing
   signatures working for existing callers if reasonably possible (add a new function
   rather than breaking the public API, unless breaking it is clearly cleaner — use
   judgment).
3. **`build-opening-cache` CLI subcommand** in `main.rs`, following the existing
   subcommand patterns (see `Commands::SolveOpening`, `Commands::Verify` for style).
4. **Load-and-wire-in** across the three consumers listed above.
5. **Correctness test** (the most important one): run a search with an empty TT, then
   run the *same* search again with the persistent cache pre-loaded (built from a
   *different*, e.g. shallower or unrelated, prior search, to make sure you're
   genuinely exercising cache hits and not just replaying the same run against itself).
   Assert **identical results** — same `Value`, and ideally spot-check that a handful
   of individual position lookups agree too. A cache is only a valid optimization if it
   never changes what the search concludes, only how fast it gets there. This mirrors
   how this codebase treats every other performance optimization in its history (see
   git log: the M6 "analytical reciprocal-count formula" commit and its permanent
   debug-mode cross-check in `retro.rs` for the pattern to follow — a cache/optimization
   earns trust by being checked against the slow-but-obviously-correct path, not by
   assertion).
6. **Staleness test**: build a cache against one version of a small test database
   (e.g. via `orchestrate::solve_all(&tmp, Some(6))` in a temp dir, matching existing
   test patterns in `opening.rs`/`play.rs`), then re-solve that same pair (changing
   nothing meaningful, but producing a fresh file with the pair's checksum
   nonetheless — or more robustly, solve a *different* small pair and swap the
   manifest), and confirm the cache is correctly detected as stale and ignored rather
   than silently trusted.
7. **Benchmark**: measure and report (in your PR description, and worth adding a line
   to `getting-started.md` or `RESULTS.md` if the numbers are compelling) the
   wall-clock difference for a representative scenario — e.g. time to first move
   suggestion in a fresh `ninemm play` session, or fresh `ninemm serve --warm` startup
   time — with vs. without a pre-built cache. This is the actual point of the feature;
   confirm it delivers before considering the task done.
8. **Docs**: update `getting-started.md` with the new `build-opening-cache` command
   (following the style of its existing command sections), and mention the cache file
   in `readme-database.md` if you think external consumers of the database directory
   would care that this file might be sitting alongside the `wdl_*.bin` files (probably
   worth a short note there, since `readme-database.md`'s whole premise is "here's
   everything in this directory and what it means").

## Testing checklist (summary)

- [ ] Round-trip: write cache, read it back, values match.
- [ ] Corrupt/truncated file: graceful fallback, no panic.
- [ ] Fingerprint mismatch: correctly detected and rejected.
- [ ] **Identical search results with vs. without a valid pre-loaded cache** (the
      core correctness property).
- [ ] `cargo test --release` (full suite) and `cargo clippy --release --all-targets`
      still clean after your changes.
- [ ] Benchmark numbers captured somewhere (PR description at minimum).

## Future extensibility notes (don't build these now, just don't paint yourself into a corner)

- If depth-aware `Value` ever gets added to `opening.rs` (see the "related but
  out-of-scope gap" above), the cache's 1-byte value field would need to grow — the
  8 spare bits in the packed key, or a version bump in the file header (`b"NMMOPEN2"`),
  give you room to do this without an awkward migration. Just be aware of it; no need
  to design for it now.
- If organic cache growth (writing back newly-explored entries after each session)
  is ever wanted, the sorted-array format described here would need to become
  merge-friendly (e.g. periodic rebuilds from an accumulated log, rather than in-place
  updates to a sorted file) — again, not needed now, just don't assume the format is
  fundamentally incompatible with that future if someone asks for it later.
