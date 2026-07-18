# Implementation Guide: Persistent Opening-Phase Cache

**Audience**: an implementer (human or model) working alone, without further context
from the authors. Follow the steps in order; each step ends with a gate you must pass
before moving on. Where this guide gives exact code, use it as given — those are the
places where a plausible-looking variation is subtly wrong.

**Read first, in this order** (all in the repo root / `src/`):

1. `design-opening-phase.md` — the design this guide implements (including its
   correction note, which points back here).
2. `src/opening.rs` — the search you are caching (~300 lines, half of it tests).
3. `src/persist.rs` — the on-disk conventions you are imitating (~250 lines).

**Scope**: one PR. New module `src/opening_cache.rs`, a ~30-line correctness fix in
`src/opening.rs`, a new CLI subcommand, small wiring changes in `src/main.rs`,
`src/server.rs`, `src/play.rs`, tests, and doc updates. No changes to the
movement-phase database, its format, `manifest.json`, or `src/retro.rs`.

## Deviations from design-opening-phase.md (read before starting)

This guide was written after a line-by-line review of the design doc against the code.
The design is implemented as written **except** for four points. Where the documents
disagree, this guide wins.

1. **The transposition table must become bound-aware first (new Step 1).** The design
   doc says "you should not need to touch `negamax`'s logic at all." That turned out
   to be wrong. `negamax` is *fail-soft alpha-beta*: the value it inserts into the TT
   after a beta cutoff is only a **lower bound** on the true value, and the value it
   inserts after failing low is only an **upper bound** — yet the TT probe returns
   every entry as if exact. Concretely, with this game's 3-valued domain, a node
   searched under the window `(-1, 0)` that finds one drawing move cuts off and stores
   `0`, even if an unexplored later move wins (true value `1`). Such windows occur
   constantly (the root is a draw, so after the first drawing root move is found, whole
   subtrees are searched under narrow windows). Today this is a *latent* bug: the
   server's long-lived TT and `play`'s per-session TT can already return "Draw" for a
   position whose true value is Win/Loss when a later `solve()` probes a polluted
   entry at full window. Persisting raw TT entries to disk would bake those wrong
   values into a file and serve them forever, and the design doc's own core
   correctness test ("identical results with vs. without cache") would not hold by
   construction. Step 1 fixes this with standard TT bound flags; it also fixes the
   pre-existing bug.
2. **The on-disk value byte encodes value + bound** (5 valid byte values, listed in
   Step 2), not a raw `i8`, as a consequence of point 1.
3. **The header gains a payload checksum** (xxh3 over the entry region). The design
   doc's format had only structural validation; a single bit-flip in a stored key
   could otherwise silently change an answer. The movement-phase database checksums
   everything; this file should too. Header grows from 24 to 32 bytes.
4. **The cache file is read with `std::fs::read`, not mmap.** The design doc asked for
   mmap, but its own load strategy immediately copies every entry into a `HashMap`,
   and the file is a few MB. The doc's mmap requirement exists to protect against
   OOM on the 17 GB movement database — which this file is not. Plain `read` is
   simpler and equally safe at this size. (The *movement* database must still be
   mmap'd everywhere, as it already is — see Step 3.)

One more clarification: the design doc's Step 6 staleness test suggests "re-solve the
same pair" to change the fingerprint. With the content-based fingerprint specified
below, re-solving a pair to *identical bytes* produces an identical fingerprint — the
cache correctly stays valid, because nothing actually changed. Use the doc's
alternative (mutate a manifest entry's checksum string) to test staleness, as Step 5
does.

## Step 0: Baseline

```sh
cargo test --release          # must pass before you change anything
cargo clippy --release --all-targets   # must be clean
```

If either fails before you've changed anything, stop and report that instead of
proceeding.

## Step 1: Bound-aware transposition table (`src/opening.rs` + ripples)

### 1a. New types in `src/opening.rs`

Add near `Value` (top of file, after the `use` block is fine):

```rust
/// How a transposition-table value relates to the true game value.
/// Fail-soft alpha-beta only proves a bound when it cuts off (Lower)
/// or fails low (Upper); probing code must not treat bounds as exact.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Bound {
    Exact,
    Lower,
    Upper,
}

/// The opening-search transposition table: canonical `(position,
/// mover_hand, opp_hand)` key → (value, bound).
pub type Tt = HashMap<(Position, u8, u8), (i8, Bound)>;
```

### 1b. Change `negamax` (exact code)

Replace the TT probe (currently `if let Some(&v) = tt.get(&key) { return v; }`) with:

```rust
    let key = tt_key(state);
    if let Some(&(v, bound)) = tt.get(&key) {
        match bound {
            Bound::Exact => return v,
            Bound::Lower if v >= beta => return v,
            Bound::Upper if v <= alpha => return v,
            _ => {}
        }
    }
    let alpha_orig = alpha;
```

`alpha_orig` **must** be captured after the probe and before the successor loop, and
must be the *unmutated* function-entry alpha (the loop mutates `alpha`; the probe
above does not).

Replace the final store (currently `tt.insert(key, best);`) with:

```rust
    // Classify what fail-soft alpha-beta actually proved. In this
    // 3-valued domain a lower bound of 1 and an upper bound of -1 are
    // the domain extremes, hence exact; the only genuinely inexact
    // stores are "0, at least" and "0, at most".
    let bound = if best >= beta && best < 1 {
        Bound::Lower
    } else if best <= alpha_orig && best > -1 {
        Bound::Upper
    } else {
        Bound::Exact
    };
    tt.insert(key, (best, bound));
    best
```

Change the two signatures from `tt: &mut HashMap<(Position, u8, u8), i8>` to
`tt: &mut Tt` (both `solve` and `negamax`). Do **not** change anything else in
`negamax` — in particular the three early returns (`total_mover() < 3`,
`placement_done()`, empty successors) must stay before/outside the TT exactly as they
are, and `play.rs`'s call must keep its full `(-1, 1)` window.

### 1c. Ripple sites (compile-error driven, all mechanical)

- `src/play.rs`: `best_placement_move`'s `tt` parameter becomes `&mut opening::Tt`;
  drop the now-unused `HashMap` import if clippy complains.
- `src/server.rs` line ~32: replace the alias body:
  `pub type Tt = crate::opening::Tt;` (keep the doc comment).
- `src/main.rs` `run_play` (~line 281): the explicit type annotation becomes
  `let mut tt = ninemm::opening::Tt::new();` (this line changes again in Step 4).
- `src/opening.rs` tests: `let mut tt = HashMap::new();` lines still compile via
  inference; leave them.

### 1d. New test in `src/opening.rs`'s `tests` module

The invariant: **reusing one TT across many `solve` calls never changes any result.**
This is exactly what the server does across requests and what the persistent cache
will institutionalize.

```rust
    #[test]
    fn tt_reuse_across_solves_never_changes_results() {
        let tmp = std::env::temp_dir()
            .join(format!("ninemm_opening_ttreuse_test_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(6)).unwrap(); // {3,3} only
        let manifest = Manifest::load(&tmp).unwrap();
        let db = load_db_up_to(&tmp, &manifest);

        // Same funnel-to-{3,3} region the existing tests use.
        let root = PlacementState {
            pos: Position::new((1 << 3) | (1 << 8), 1 << 16),
            mover_hand: 1,
            opp_hand: 2,
        };
        // Collect root, its children, and grandchildren.
        let mut states = vec![root];
        for s in successors(&root) {
            states.push(s);
            states.extend(successors(&s));
        }
        // Solve all of them sharing ONE table, in order...
        let mut shared = Tt::new();
        let shared_results: Vec<i8> =
            states.iter().map(|s| negamax(s, -1, 1, &db, &mut shared)).collect();
        // ...and each against the TT-free, pruning-free reference.
        for (s, &r) in states.iter().zip(&shared_results) {
            assert_eq!(r, brute_force(s, &db), "shared-TT solve diverged at {s:?}");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }
```

### Gate 1

`cargo test --release` fully green (including the pre-existing
`negamax_matches_brute_force_on_a_few_ply_subtree`), clippy clean.

## Step 2: The cache module (`src/opening_cache.rs`)

Register it in `src/lib.rs` (`pub mod opening_cache;`, alphabetical order).

### 2a. File format (normative)

Path: `<dir>/opening_cache.bin` (same `dir` as the `wdl_*.bin` files; expose
`pub const CACHE_FILENAME: &str = "opening_cache.bin";`).

All integers native-endian, same policy (and same justification comment) as
`persist::as_bytes`.

| offset | size | content |
|---|---|---|
| 0 | 8 | magic + version: the ASCII bytes `NMMOPEN1` |
| 8 | 8 | database fingerprint, `u64` (§2c) |
| 16 | 8 | entry count `n`, `u64` |
| 24 | 8 | payload checksum: `xxh3_64` of bytes `32 .. 32 + 9*n` |
| 32 | 9×n | entries, strictly ascending by packed key |

Entry: `[8 bytes packed key][1 byte packed value]`.

**Packed key** (exact code — note `Position` is `pub struct Position(pub u64)` whose
`u64` already holds white in bits 0..24 and black in bits 24..48, so the design doc's
layout is literally `pos.0` plus the hands):

```rust
fn pack_key(pos: Position, mover_hand: u8, opp_hand: u8) -> u64 {
    debug_assert!(mover_hand <= 9 && opp_hand <= 9);
    pos.0 | ((mover_hand as u64) << 48) | ((opp_hand as u64) << 52)
}

/// None ⇒ the key is structurally invalid (treat the file as corrupt).
fn unpack_key(key: u64) -> Option<(Position, u8, u8)> {
    if key >> 56 != 0 {
        return None; // spare bits must be zero in version 1
    }
    let white = (key & 0xFF_FFFF) as u32;
    let black = ((key >> 24) & 0xFF_FFFF) as u32;
    let mover_hand = ((key >> 48) & 0xF) as u8;
    let opp_hand = ((key >> 52) & 0xF) as u8;
    if white & black != 0 || mover_hand > 9 || opp_hand > 9 {
        return None;
    }
    Some((Position::new(white, black), mover_hand, opp_hand))
}
```

**Packed value byte** — exactly five values are legal; anything else means the file is
corrupt:

| byte | meaning |
|---|---|
| 0 | Exact −1 (Loss) |
| 1 | Exact 0 (Draw) |
| 2 | Exact 1 (Win) |
| 5 | Lower bound 0 ("at least a draw") |
| 9 | Upper bound 0 ("at most a draw") |

(Encoding rule behind the table: `(value + 1) | (bound_code << 2)` with Exact=0,
Lower=1, Upper=2; Step 1's store logic guarantees ±1 are always Exact, so only these
five bytes are ever produced. Decode by exhaustive `match`, rejecting everything
else.)

### 2b. Public API

```rust
/// xxh3 of "{w}-{b}:{xxh3};" for every manifest entry, sorted by (w, b).
/// Content-based: identical database bytes ⇒ identical fingerprint,
/// regardless of when or how often pairs were re-solved.
pub fn db_fingerprint(manifest: &Manifest) -> u64;

/// Filter `tt` to entries with mover_hand + opp_hand >= min_hand_sum,
/// pack, sort ascending by key, and write atomically (build the full
/// byte buffer in memory, write to "<file>.tmp", fs::rename into place —
/// persist::write_subspace's pattern). Returns (entries_written, file_bytes).
pub fn write_cache(dir: &Path, fingerprint: u64, tt: &Tt, min_hand_sum: u8)
    -> anyhow::Result<(usize, u64)>;

/// Load the cache into a fresh TT. EVERY failure path — file missing,
/// short/oversized file, bad magic, fingerprint mismatch, checksum
/// mismatch, unsorted or invalid entry — returns an empty TT and must
/// never panic or return Err. Missing file: silent (normal before the
/// first build). Fingerprint mismatch: eprintln! that the cache is stale
/// (database changed since it was built) and is being ignored. Any other
/// failure: eprintln! that the cache is corrupt and is being ignored.
/// Success: eprintln! "loaded {n} opening-cache entries from {path}".
pub fn load_or_empty(dir: &Path, manifest: &Manifest) -> Tt;
```

`min_hand_sum` is a parameter (not hardcoded from `max_ply`) because the small test
databases only reach states with hand sums 0–3: tests pass `0`, the CLI passes
`18 - max_ply`. Ply arithmetic sanity check: every ply-`p` state has
`mover_hand + opp_hand == 18 - p` (each placement decrements exactly one hand), so
"ply ≤ 8" ⇔ "hand sum ≥ 10".

Load-side validation order: length ≥ 32 → magic → fingerprint (the *stale* case, check
before spending time on the payload) → `n` and exact length `32 + 9*n` → payload
checksum → per-entry `unpack_key` + value-byte decode + strictly-ascending keys.
Insert as `((pos, mover_hand, opp_hand), (value, bound))`. Keys come out already
canonical because they were harvested from a TT keyed by `tt_key` — do **not**
re-canonicalize on load, and do not expose or modify `tt_key` (it stays private;
nothing in this task needs it).

### 2c. Unit tests (in `opening_cache.rs`; no game database needed — synthetic TTs)

Follow the existing temp-dir naming pattern
(`std::env::temp_dir().join(format!("ninemm_opcache_test_{}", std::process::id()))`).

1. **Round-trip**: build a small `Tt` by hand (a handful of disjoint
   `Position::new(w, b)` boards, assorted hands, all five value/bound combinations),
   `write_cache` with `min_hand_sum` 0, `load_or_empty` with the same manifest →
   equal to the input map. Also assert the file's entry keys are sorted (read the raw
   bytes back and check).
2. **Threshold filter**: entries below `min_hand_sum` are absent from the file.
3. **Missing file**: empty dir → empty TT (and no panic).
4. **Corrupt payload**: flip one byte in the entry region → empty TT.
5. **Corrupt values**: rewrite one value byte to `7` and fix nothing else → empty TT
   (caught by checksum; also write a file with a correct checksum over an invalid
   value byte if you want the decoder path covered — easiest by computing the header
   checksum yourself over a hand-built payload).
6. **Truncated file / bad magic**: empty TT.
7. **Staleness**: two synthetic `Manifest`s differing only in one entry's `xxh3`
   string → different `db_fingerprint`; a cache written under the first loads empty
   under the second and non-empty under the first.
8. **Fingerprint determinism**: same entries in different `manifest.entries` order →
   same fingerprint.

### Gate 2

`cargo test --release` green, clippy clean.

## Step 3: CLI subcommand (`src/main.rs`)

Add to `Commands` (mirror `SolveOpening`'s doc-comment style):

```rust
    /// Run the empty-board opening solve and persist its shallow
    /// transposition-table entries to db/opening_cache.bin, so later
    /// `play` / `serve` / `solve-opening` runs start warm (see
    /// design-opening-phase.md). Requires the full 49-subspace database.
    BuildOpeningCache {
        #[arg(long, default_value = "db")]
        dir: PathBuf,
        /// Keep entries within this many plies of the empty board
        /// (a ply-p state has mover_hand + opp_hand == 18 - p; the
        /// default 8 matches Gasser's cutoff).
        #[arg(long, default_value_t = 8)]
        max_ply: u8,
    },
```

Handler:

1. `anyhow::bail!` if `max_ply > 18`.
2. Load the manifest; fail with the same all-49-subspaces-present check and
   error-message style as `run_play` (copy that loop; `SolveOpening` doesn't check,
   but a partial build would just crash deep in the search — fail early instead).
3. Map the database with `persist::mmap_subspace` + `db.insert_mmap`, exactly as the
   `SolveOpening` handler does. **Never** load subspaces as owned `Vec`s here — that
   is the OOM the mmap path exists to prevent.
4. `let mut tt = opening_cache::load_or_empty(&dir, &manifest);` — a still-valid
   existing cache legally warm-starts a rebuild (entries are exact-or-bounded facts
   independent of search order), making re-builds with a different `--max-ply` cheap.
5. Time `opening::solve(&PlacementState::initial(), &db, &mut tt)` and print the root
   value and elapsed time.
6. `let fp = opening_cache::db_fingerprint(&manifest);` then
   `write_cache(&dir, fp, &tt, 18 - max_ply)`.
7. Print: entries written, file size, total wall-clock. If entries written exceeds
   ~10 million, also print a warning to investigate before trusting the file (the
   design doc's estimate is low-hundreds-of-thousands; a huge count suggests a
   packing/canonicalization bug).

## Step 4: Wire the three consumers

- **`src/main.rs` `run_play`** (~line 281): replace the TT construction with
  `let mut tt = ninemm::opening_cache::load_or_empty(dir, &manifest);` (`manifest` is
  already in scope from the completeness check above it).
- **`src/main.rs` `SolveOpening` handler**: after mapping the database, replace
  `opening::solve_from_empty_board(&db)` with the load-then-solve pair, keeping the
  existing timing prints:
  ```rust
  let mut tt = ninemm::opening_cache::load_or_empty(&dir, &manifest);
  let t0 = Instant::now();
  let value = opening::solve(&opening::PlacementState::initial(), &db, &mut tt);
  ```
  (`solve_from_empty_board` itself stays in `opening.rs` — it's public API and
  correct, just no longer used here.)
- **`src/server.rs`**: `load_db` currently drops the `Manifest` it parses. Add
  `pub manifest: Manifest` to `Loaded`, populate it in `load_db` (move it into the
  struct after the loop that borrows it), fix any test constructors of `Loaded` by
  adding `manifest: Manifest::default()`. Then in `serve()` replace
  `let mut tt: Tt = HashMap::new();` with
  `let mut tt: Tt = opening_cache::load_or_empty(dir, &loaded.manifest);`.
  The `--warm` block stays as-is (it now warms *on top of* the cache). Note the free
  property: with `--allow-partial`, the partial manifest's fingerprint can't match a
  cache built against the full database, so the cache is automatically ignored —
  no extra `loaded.complete` check needed.

## Step 5: Integration tests (in `src/opening.rs`'s `tests` module, which already has
`brute_force` and `load_db_up_to`)

**Identical-results test** — the core correctness property. Using the
`orchestrate::solve_all(&tmp, Some(6))` `{3,3}` database and the same funnel states as
Step 1d:

1. Solve state `A` (`pos: Position::new((1 << 3) | (1 << 8), 1 << 16)`, hands
   `(1, 2)`) with a fresh `Tt` → `v_a`; assert it equals `brute_force(&A, &db)`.
2. `write_cache(&tmp, db_fingerprint(&manifest), &tt_from_step_1, 0)` — note
   `min_hand_sum = 0`, since these test states have hand sums ≤ 3.
3. `let mut cached = load_or_empty(&tmp, &manifest);` assert `!cached.is_empty()`.
4. Pick `B` = the first element of `successors(&A)` — a state whose subtree overlaps
   the cached search but which wasn't the previous root, per the design doc's "not
   just replaying the same run" requirement. Assert
   `solve(&B, &db, &mut cached) == solve(&B, &db, &mut Tt::new())`, and that both
   equal `from_i8(brute_force(&B, &db))`.
5. Re-load a second copy of the cache and assert `solve(&A, ...)` on it returns `v_a`.

**Staleness end-to-end** is already covered synthetically in Step 2c(7); no
database-backed version needed.

### Gate 5

`cargo test --release` green, `cargo clippy --release --all-targets` clean,
`cargo fmt` produces no diff.

## Step 6: Docs

- `getting-started.md`: new `## Build the opening cache` section (match the existing
  sections' voice: what the command does, the exact command line, what output to
  expect, and that stale/corrupt caches are ignored automatically — delete the file
  or re-run the build after re-solving the database).
- `readme-database.md`: short section documenting `opening_cache.bin` alongside the
  `wdl_*.bin` files: what it is, the format table from Step 2a, that it is derived
  data (safe to delete, rebuild with `ninemm build-opening-cache`), and that its
  header fingerprint ties it to the exact manifest contents it was built from.
- `RESULTS.md`: only if you complete Step 7 — add the measured numbers to the
  "Opening" section.

## Step 7: Full-scale run (attempt it, but time-box it)

Context you need before judging results here: per `RESULTS.md`'s "Opening… in
progress" section, the full empty-board search **has never yet completed on this
machine** — the 17 GB database barely fits in RAM and the search's move ordering is
not cache-locality-aware, so it pages heavily. This cache amortizes that cost across
runs but does not reduce a single run's cost. Consequences for you:

- Run `cargo build --release`, then
  `./target/release/ninemm build-opening-cache --dir db` **in the background with
  output to a log**, and check on it periodically. If it hasn't finished within your
  available budget, kill it and note honestly in the PR that the build step was
  exercised only at test scale (this is acceptable; the small-scale correctness
  evidence is the acceptance bar).
- If it **does** finish: the printed root value is expected to be `Draw` (Gasser's
  published result). If it prints anything else, **stop and report it prominently** —
  do not ship the PR as a routine cache feature. It would mean either the Step 1
  bound fix changed the outcome (the previous number was computed with the unsound
  TT, so this is possible and would itself be a significant finding) or a bug in your
  changes.
- Then benchmark the actual point of the feature: time `ninemm solve-opening --dir db`
  and/or `ninemm serve --dir db --warm` startup with the cache file present vs.
  temporarily renamed away, and put both numbers in the PR description (and
  `RESULTS.md` if compelling).

## Acceptance checklist

- [ ] Step 0 baseline was green before any changes.
- [ ] TT probe honors Exact/Lower/Upper; store classifies via `alpha_orig`/`beta`
      exactly as specified; no other `negamax` logic changed.
- [ ] `tt_reuse_across_solves_never_changes_results` passes.
- [ ] Round-trip, threshold, missing, corrupt (payload + value-byte), truncated,
      bad-magic, staleness, fingerprint-determinism unit tests pass.
- [ ] Every bad-cache path degrades to an empty TT with a log line — nothing panics,
      nothing errors out.
- [ ] `build-opening-cache` refuses partial databases, mmaps the movement DB, prints
      count/size/time, warns on absurd entry counts.
- [ ] All three consumers (`play`, `solve-opening`, `serve`) load the cache;
      `--allow-partial` serving ignores it via fingerprint mismatch.
- [ ] Identical-results integration test passes (cache built from a *different* prior
      search, per Step 5).
- [ ] `cargo test --release`, `cargo clippy --release --all-targets`, `cargo fmt`
      all clean.
- [ ] Docs updated; PR describes the Step 1 correctness fix separately from the cache
      feature, and reports Step 7 results or honestly states the build was not run to
      completion.

## Pitfalls (each of these is a mistake a reasonable implementer might make)

- **Do not** store or probe TT entries for the early-return states
  (`total_mover() < 3`, `placement_done()`, empty successors) — the existing control
  flow already returns before the TT; keep it that way.
- **Do not** re-canonicalize keys on load or expose `tt_key` — harvested keys are
  already canonical; a second canonicalization is wasted work at best and a
  divergence risk at worst.
- **Do not** widen `play.rs`'s per-successor window or "optimize" its loop — it must
  stay `negamax(&succ, -1, 1, ...)`.
- **Do not** load movement-phase subspaces as owned `Vec`s anywhere new — mmap only
  (`server::load_db`'s owned-load is pre-existing and out of scope; leave it).
- **Do not** add cache entries to `persist::Manifest` / `manifest.json` — the cache
  is self-describing via its header.
- **Do not** hardcode the hand-sum threshold inside `write_cache` — tests need `0`.
- Native-endian throughout, matching `persist.rs` — do not "fix" this to
  little-endian in one place only.
- The five legal value bytes are exhaustive: decode with a `match` that rejects
  everything else; do not decode arithmetically and skip validation.
- `alpha_orig` is the function-entry alpha captured *after* the probe and *before*
  the loop — not the mutated loop variable, and not captured before the probe's
  early returns (placement of the `let` matters only relative to the loop, but keep
  it where this guide puts it for clarity).
- Writing goes through a `.tmp` file plus `fs::rename` — never write the final path
  directly (a crash mid-write must not leave a plausible-looking half file; the
  checksum would catch it, but don't rely on that alone).
