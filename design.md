# Solving Nine Men's Morris — Design

Goal: **strongly solve Nine Men's Morris** — compute the game-theoretic value (and a
perfect-play policy) for every reachable position, reproduce Gasser's result (the game
is a draw), and produce a compact database from which a perfect player can be driven
in real time.

The design follows Gasser (*Solving Nine Men's Morris*, Games of No Chance, MSRI 1996)
in structure — retrograde analysis for the movement phases, forward search for the
placement phase — but exploits 30 years of hardware progress: the entire state space
fits in RAM, so all of Gasser's disk-I/O contortions (bit-packed bound databases,
proving the draw from only three subspaces, ply-16-only transposition tables) disappear.
We compute *exact* values for everything instead of bounds.

---

## 1. The game and rule decisions

- 24 board points on three concentric squares connected by four midlines; 16 mills
  (rows of three); every point lies in exactly 2 mills; the mill graph has degree
  2–4 adjacency.
- **Opening** (plies 1–18): players alternately place their 9 stones on vacant points.
- **Midgame**: slide a stone to an adjacent vacant point.
- **Endgame**: a player reduced to 3 stones may jump to any vacant point.
- Closing a mill removes one opponent stone that is not in a mill.
- Loss: fewer than 3 stones, or no legal move. Repetition: draw.

Ambiguous rules are fixed exactly as in Gasser, so results are comparable:

1. Closing two mills with one move still removes only **one** stone.
2. If all opponent stones are in mills, a mill closure may remove **any** stone.

Repetition-as-draw is modeled implicitly: retrograde analysis assigns *draw* to every
position where neither side can force a win; under optimal play such positions cycle
forever, which is exactly the repetition rule's outcome. No repetition detection is
needed in the solver.

## 2. Overall architecture

Two phases with fundamentally different graph structure, hence two methods:

| Phase | Graph | Method |
|---|---|---|
| Mid/endgame (all stones placed) | cyclic, ~10¹⁰ states | retrograde analysis over per-material subspaces |
| Opening (18 plies, acyclic) | ~10¹⁰ states but only one root value needed | alpha-beta from the empty board probing the databases |

This is Gasser's split, and it remains the right one: the opening is acyclic and only
the empty board's value matters, so building full opening databases is unnecessary
(they'd be ~2.7×10¹⁰ states after symmetry — an optional extension, §9).

## 3. State space

A mid/endgame position is `(white bitset, black bitset, side to move)` with each side
having 3–9 stones. Two reductions:

**Color normalization.** Swapping colors and side-to-move yields an equivalent game, so
every stored position has *White to move*. Values are always "for the side to move".

**Board symmetry.** The mill graph has a 16-element automorphism group: the dihedral
group of the square (8 elements) × swapping inner and outer rings (2). (The 3-vs-3
subspace admits more symmetry because adjacency stops mattering when both sides jump;
we deliberately ignore that — the subspace is tiny.)

Exact counts (computed): Σ<sub>w,b∈[3,9]</sub> C(24,w)·C(24−w,b) =
**142,430,121,724** raw states, ≈ **8.9 × 10⁹** after 16-fold symmetry
(Gasser: 7.67×10⁹ after additionally excluding some unreachable states; we keep
harmless unreachable states as slack rather than complicating the index). At 1 byte
per state the full database is ≈ 9 GB — it fits in RAM.

### Subspaces and dependency order

States are partitioned into 49 ordered subspaces `N(w,b)` (stones of the side to move,
stones of the opponent). Transitions:

- quiet move: `N(w,b) → N(b,w)` (perspective flips),
- mill closure: `N(w,b) → N(b−1,w)`.

So the unordered pair `{a,b}` (i.e. `N(a,b)` ∪ `N(b,a)`) is a strongly-connected unit
whose cross-edges leave only to pairs with fewer total stones. Solve the 28 unordered
pairs bottom-up in ascending total-stone order: `{3,3}, {4,3}, {4,4}, {5,3}, …, {9,9}`
(Gasser's Figure 4 DAG). Cross-subspace successors are always already solved.

### Indexing (perfect-enough hash)

Per subspace `N(w,b)`, an index built from combinatorial ranking with symmetry folded
into the *white* configuration only:

1. Enumerate all C(24,w) white stone sets; keep only those that are the lexicographic
   minimum of their 16-symmetry orbit ("canonical white sets"). Precompute a sorted
   table mapping canonical white set → dense rank `rW` (and its inverse). Size ≤
   C(24,9)/16 ≈ 82k entries — trivial.
2. For a full position, canonicalize by applying the symmetry that minimizes the white
   set (ties broken by minimizing the black set among the white-stabilizing symmetries).
3. The black set occupies `24−w` remaining points; rank it by standard combinatorial
   number system into `rB ∈ [0, C(24−w, b))`.
4. `index = rW · C(24−w, b) + rB`.

When the canonical white set has a nontrivial stabilizer, a few black-set variants of
the same position occupy distinct slots — wasted but harmless slack (same trade Gasser
made: 7.67G states in 9.07G slots). The index is fast (a few table lookups + PEXT-style
bit compaction), invertible (needed for "iterate all states" scans and unranking), and
dense enough.

## 4. Values and depth

One byte per state during solving, exactly Gasser's Val/Count union (Table 1):

- `0 = loss in 0, 1 = win in 1, 2 = loss in 2, 3 = win in 3, …` — value with depth,
- high codes `255, 254, …` = "still draw, k successors not yet proven wins".

Depth is **DTC-flavored within the pair**: a mill-closure move is a "conversion" into
an already-solved smaller pair, so depths measure plies until conversion or terminal.
This keeps depths small (known maximum ≈ 187 plies to mill closure, Gasser Fig. 10 —
fits a byte with the ~250 codes available) and still yields perfect play: following
minimal-DTC winning moves strictly decreases (depth, material) lexicographically, so
wins terminate.

Final artifact per subspace: the raw byte array (WDL + depth), plus an optional
2-bit-packed pure-WDL export (~2.3 GB total) for distribution.

## 5. Retrograde analysis (per unordered pair {a,b})

Working set: the Val/Count arrays for `N(a,b)` and `N(b,a)` (≤ 2·1.06 G entries ≈ 2.1 GB
for the worst pair {9,8} — comfortable), plus read-only mmapped solved arrays of the
capture-successor pairs.

**Initialization** (single parallel scan over all states of the pair):

1. Side to move is blocked (no legal move) → **loss in 0**. (Only possible with ≥4
   stones; 3 stones can always jump.)
2. Side to move can close a mill *and* the opponent has 3 stones → **win in 1**
   (successor would leave the opponent with 2 stones — outside the state space, so it
   is resolved here, as in Gasser).
3. Otherwise, evaluate all *capture* successors (they live in solved smaller pairs;
   look their values up): if any is a loss for the opponent → tentative **win in d+1**;
   record how many non-capture successors exist and whether all capture successors are
   opponent-wins. All decided states go into the work queue.
   - Gasser's extra seeds (two open mills, forced unblocking, 2-ply losses for the
     3-stone player) are pure optimizations of this rule and fall out automatically
     from the general "capture successors are already solved" initialization.
4. Everything else starts as *draw* with `Count = number of unknown (non-capture)
   successors` — stored lazily Gasser-style: Count is only materialized on first
   decrement (encoded in the high byte codes).

**Propagation** (Gasser's algorithm, Table 2, parallelized): pop a decided state `s`,
generate its **within-pair predecessors** by reverse quiet moves (reverse slide; reverse
jump if the mover has 3 stones; un-closing of mills does not occur within-pair since
captures are cross-pair — reverse moves never add/remove stones):

- `s` is a loss (for its mover) → each undecided predecessor becomes **win in
  depth(s)+1**, enqueue.
- `s` is a win → decrement predecessor's Count; at zero, all its successors are proven
  wins for the opponent → **loss in max-successor-depth+1**, enqueue.

Iterate until the queue empties; remaining states are true draws. Loss depths need the
*maximum* over successors, obtained for free: the *last* proving decrement carries the
final (largest, due to BFS-like level order) depth — we process the queue in depth
levels (two-queue frontier alternation) to make that exact.

**Parallelism.** Frontier-based level processing with `rayon`: each level's states are
processed in parallel; Count decrements use `AtomicU8` `fetch_sub`; newly decided
states are collected into per-thread buffers and merged into the next frontier.
Determinism per level order makes depths well-defined and runs reproducible.

**Cost estimate.** ~1.4×10¹¹ raw state-visits worth of work compressed 16× ≈ 10¹⁰
state initializations plus a few edge-traversals each (~30 avg successors ×
predecessors); order 10¹¹–10¹² cheap bit ops. On 8–16 modern cores: expect **a few
hours**, dominated by the {9,8}, {9,9}, {8,8} pairs. (Gasser: years of wall-clock across
1989–1993 machines.)

## 6. Opening search

With all 49 subspace arrays resident (9 GB mmapped/loaded — no I/O bottleneck, unlike
Gasser's 72 MB machine), the empty board is solved by plain **alpha-beta over the
18-ply placement DAG**:

- State: `(white set, black set, stones left to place per side)`; capture moves as per
  rules; at ply 18 (all stones placed or a side already below 3) probe the databases /
  terminal rules for exact values.
- Transposition table keyed on canonicalized placement states (placement states repeat
  heavily across move orders — the DAG has ~10⁹ distinct ply-≤18 canonical nodes but
  alpha-beta with a TT visits a tiny fraction; Gasser evaluated only ~20k ply-8 nodes).
- Iterative deepening unnecessary (fixed depth); move ordering: mills/captures first,
  then center-symmetric points.
- Because our databases hold *exact* values, one search yields the exact root value
  (expected: **draw**) — no upper/lower-bound double search needed.
- Cache all ply-≤8 evaluations to disk (Gasser's "intermediate database") so the
  interactive player answers instantly in the opening.

## 7. Perfect player

`play` mode: for any position, enumerate legal moves, canonicalize each successor,
probe its subspace array (or run the opening search for placement-phase positions),
pick: any winning move with minimal depth ▸ any drawing move ▸ losing move with
maximal depth. This is the end-to-end demo and the strongest practical test.

## 8. Verification (Gasser found real bugs *and* hardware errors — take this seriously)

1. **Forward consistency scan** (independent code path from the retrograde solver):
   for *every* state, recompute `value(s) = minimax over successors' stored values`
   and check depth arithmetic. Fully parallel, ~same cost as one solve pass.
2. **Independent oracle on small subspaces**: a naive memoized forward solver
   (no symmetry, no subspace machinery, written separately) fully solves `{3,3}`
   (~2.7 M raw states) and spot-checks `{4,3}`, `{4,4}`; results must match exactly.
3. **Cross-checks against published results**: initial position is a draw; Gasser
   Fig. 9 (a 3-3 position lost in 26 plies), Fig. 10 (8-7 win, mill closure in 187),
   Fig. 12–14 opening evaluations; win-percentage statistics per subspace vs. his
   Fig. 11; and the malom project's published counts where available.
4. **Integrity**: per-file xxhash checksums; the solve is deterministic, so a re-run
   of any pair must reproduce identical bytes.
5. **Self-play soak**: perfect-play self-play from random reachable positions must
   never see a stored win fail to progress (win-depth must strictly decrease at
   conversions-or-better) — a cheap runtime invariant checker.

## 9. Optional extensions (explicitly out of core scope)

- **Full opening databases** via backward induction over the acyclic placement DAG
  (ply 18 → 0), keyed `(sets, placed-counts)`: ≈ 2.7×10¹⁰ post-symmetry states — ~27 GB;
  feasible on a 64 GB machine, gives instant perfect play everywhere with no search.
- **Ultra-strong play** (Gévay & Danner): among equal-value moves prefer those
  maximizing opponent error chances.
- 2-bit WDL distribution build; reachability filtering for exact state counts.

## 10. Technology choices

- **Language: Rust** (stable). Reasons: C-class codegen for the bit-twiddling inner
  loops, fearless multithreading for the atomic-heavy retrograde scan, `memmap2` for
  zero-copy database access, strong testing story. No GC pauses, no undefined behavior.
- **Crates**: `rayon` (data parallelism), `memmap2` (mmap), `clap` (CLI), `anyhow`,
  `xxhash-rust` (checksums), `proptest` (property tests), `criterion` (benches).
- **Board representation**: 48-bit position in a `u64` (white = bits 0–23, black =
  bits 24–47); precomputed tables: 24 adjacency masks, 16 mill masks, point→2-mill
  map, 16 symmetry permutations (applied as unrolled bit gathers), rank/unrank
  binomial tables.
- **Hardware target**: any 64-bit machine with ≥ 16 GB RAM (per-pair working set
  ≤ ~2.5 GB + read-only mmapped dependencies; the OS pages those); ≥ 32 GB keeps
  everything hot. Full run budget: single desktop, hours.
- **Storage**: `db/` directory, one file per ordered subspace `wdl_w_b.bin` + a
  JSON manifest (sizes, checksums, solve metadata, code version).
