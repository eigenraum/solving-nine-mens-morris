# Using the Database From Another Program

This document specifies the on-disk database format completely enough that an
external program — in any language — can consult it to play perfect Nine Men's Morris,
**without** depending on this repository's code. Treat the database directory
(`wdl_*.bin` files + `manifest.json`) as the artifact; everything else here is the spec
for reading it.

**Scope**: the database covers the **movement/endgame phase only** — any position where
both sides have already placed all 9 of their stones (ply 19 onward; both sides finish
placement simultaneously at ply 18, since placement strictly alternates). It does *not*
cover the placement/opening phase (plies 1–18). If you need perfect opening play too,
you need your own 18-ply search that probes this database at the placement/movement
boundary (see `design.md` §6 for the approach we used) — or you can simply play
reasonably during the opening and switch to consulting this database from move 19
onward, since Gasser's result (and ours, independently reproduced) is that optimal
opening play is highly drawish anyway.

## 1. Board geometry

24 points on three concentric rings of 8 (outer=0, middle=1, inner=2), connected by
adjacency edges within each ring and four "spoke" lines crossing all three rings.

**Point numbering.** Point `p = ring*8 + i` for `ring` in `0..3`, `i` in `0..8`. Within
a ring, `i` runs clockwise starting at a corner; even `i` (0,2,4,6) are corners, odd `i`
(1,3,5,7) are edge midpoints.

**Adjacency** (`p` adjacent to `q`):
- Ring-cycle: `ring*8+i` is adjacent to `ring*8+((i+1)%8)` and `ring*8+((i+7)%8)`, for
  every ring.
- Spokes: for odd `i` only, `0*8+i` is adjacent to `1*8+i`, and `1*8+i` is adjacent to
  `2*8+i`. (Outer and inner rings are never directly adjacent — only via the middle
  ring.)

This gives 12 corner points of degree 2, 8 outer/inner-ring midpoints of degree 3, and 4
middle-ring midpoints of degree 4 (32 edges total).

**Mills** (lines of 3 — closing one lets you capture): 16 total.
- 12 "ring" mills: for each ring, `{ring*8+i, ring*8+i+1, ring*8+i+2}` for `i` in
  `{0,2,4,6}` (indices mod 8).
- 4 "spoke" mills: `{0*8+i, 1*8+i, 2*8+i}` for `i` in `{1,3,5,7}`.

Every point belongs to exactly 2 mills.

**Standard notation** (optional, for display only — not needed for the index scheme):
point `p = ring*8+i` maps to file/rank `a1`–`g7` via half-width `h = 3-ring`, then
`(col,row)` from `i` as: `i=0:(4-h,4+h)`, `i=1:(4,4+h)`, `i=2:(4+h,4+h)`,
`i=3:(4+h,4)`, `i=4:(4+h,4-h)`, `i=5:(4,4-h)`, `i=6:(4-h,4-h)`, `i=7:(4-h,4)`, with
column `1..7` → letters `a..g`.

## 2. Position representation and the "side-to-move" convention

**Every stored value is for the side to move**, regardless of which physical color
(black/white stone) is actually on move. Concretely: a position is a pair of 24-bit
point sets, `(mover, opponent)`, always disjoint. When you look up a real game
position, if it's Black's turn, treat Black's stones as `mover` and White's as
`opponent` — the returned value is Black's game-theoretic outcome, not White's.

## 3. The 16-element symmetry group

The mill graph has 16 automorphisms: rotations/reflections of the ring position (8,
the dihedral group of the square) times swapping the outer and inner rings (2; the
middle ring is fixed, since its points have degree 4 while outer/inner ring midpoints
have degree 3 — only outer↔inner is symmetric).

Point `p = ring*8 + i` maps under symmetry `(a, b, s)` to `ring'*8 + i'` where:
```
i' = (a*i + b) mod 8
ring' = s ? (2 - ring) : ring
```
for `a` in `{1, -1}`, `b` in `{0, 2, 4, 6}`, `s` in `{0, 1}` — all 16 combinations
(`a=-1` gives a reflection; `b` gives a rotation by `b/2` quarter turns).

Applying a symmetry to a position applies the point permutation to both the `mover` and
`opponent` point sets independently.

**Canonical form**: of the 16 symmetric images of a position, the canonical one is
whichever minimizes `(mover_bits, opponent_bits)` compared as a pair of integers with
`mover` as the primary key (i.e. minimize `mover`'s bitmask value first; break ties by
minimizing `opponent`'s bitmask value among symmetries that achieve that minimum
`mover`).

## 4. Subspace indexing (the near-perfect hash)

Positions are partitioned into **49 ordered subspaces** by `(w, b)` = (mover stone
count, opponent stone count), each in `3..=9`. Each ordered subspace is one `.bin`
file. Within a subspace, a canonical position maps to a dense integer index as follows.

### 4.1 Canonical white-set table

For each stone count `n` in `3..=9`, build (once, algorithmically — this is derived
data, not something you need shipped separately) the sorted list of all `n`-point
subsets that are themselves the minimum of their own 16-symmetry orbit (i.e., a subset
`S` qualifies iff, for all 16 symmetries, the transformed subset is `>= S` as a 24-bit
integer). Concretely:

```
for mask in 0 .. 2^24:
    if popcount(mask) != n: continue
    if min over all 16 symmetries of apply(sym, mask) == mask:
        canonical_sets[n].append(mask)
canonical_sets[n].sort()
```

`canonical_sets[n][r]` for rank `r` gives the `r`-th canonical `n`-point set in
ascending numeric order. (Sizes: n=3→158, n=4→757, n=5→2830, n=6→8774, n=7→22188,
n=8→46879, n=9→82880 canonical sets, matching this implementation's tables exactly —
useful as a build sanity check.)

### 4.2 Ranking a position

Given a position `(mover, opponent)` with `mover_count = w`, `opponent_count = b`:

1. Canonicalize the position (§3) to get `(mover', opponent')`.
2. `mover'` must appear in `canonical_sets[w]` (this is guaranteed by construction —
   canonicalizing always lands on a fixed point of the white-set canonicalization).
   Binary-search it to get `white_rank`.
3. Rank `opponent'` as a `b`-element subset of the 24-w points *not* in `mover'`, using
   the standard combinatorial number system: number the available points in ascending
   order `0, 1, ..., 23-w` (skipping points in `mover'`), then
   `black_rank = sum over the b chosen points, sorted ascending as compacted indices
   c_0 < c_1 < ... < c_{b-1}, of C(c_j, j+1)` (binomial coefficient; standard colex
   ranking — see any reference on the combinatorial number system, e.g. Knuth TAOCP
   4A §7.2.1.3).
4. `index = white_rank * C(24-w, b) + black_rank`.

### 4.3 A subtlety: "wasted" slots

When a canonical `mover'` set has a nontrivial symmetry stabilizer (some non-identity
symmetry maps it to itself), several `black_rank` values in the file correspond to
raw positions that are *not* the true canonical representative of their equivalence
class — they're simply never written to (they retain the default "unknown" value,
`0xFFFF`, forever). This is intentional slack — do not treat an all-`0xFFFF` region as
an error. If you always canonicalize before indexing (as specified above), you will
never read from a wasted slot for a position that's actually reachable.

## 5. Value encoding

Each subspace file is a raw array of **little-endian `u16`** values (2 bytes per
index, native byte order on x86_64/aarch64 — this format is not designed to be
portable to big-endian platforms), one per index computed as in §4, `size(w,b)`
entries where `size(w,b) = len(canonical_sets[w]) * C(24-w, b)`.

- `0xFFFF` (`u16::MAX`) = **draw**.
- Any other value `d` is a **depth-coded win/loss for the side to move**:
  - `d` even → **loss** in `d` plies.
  - `d` odd → **win** in `d` plies.
  - (This parity rule always holds: each ply flips the mover and increases depth by
    exactly 1, so win/loss strictly alternate with depth by induction from the depth-0
    base case, "no legal moves" = loss in 0.)

To find the best move from a position: enumerate legal moves (§6), for each successor
look up its value (successors with the opponent reduced below 3 stones are an implicit
**loss in 0** for whoever's to move there — not represented in any file, since fewer
than 3 stones is never a stored subspace), and prefer, in order: any successor that is
a loss for its mover with the *smallest* depth (gives you a win in `depth+1`); else any
successor that is a draw; else the successor that is a win for its mover with the
*largest* depth (delays your loss as long as possible).

## 6. Rules you need to reimplement move generation

To actually drive a game (not just look up values), you need the movement-phase rules:

- **Slide**: move one of your stones to an adjacent (§1) empty point. If you have
  exactly 3 stones, you may instead **jump** to *any* empty point.
- **Mill closure and capture**: if your move causes three of your stones to occupy a
  full mill line, remove one opponent stone. The opponent stone removed must not itself
  be part of one of the opponent's own mills, *unless all of the opponent's stones are
  in mills*, in which case any may be removed. (Two mills closed by one move still
  removes only one stone — this and the previous rule are Gasser's fixed conventions,
  reproduced here for consistency with the stored values.)
- **Terminal conditions**: a side with fewer than 3 stones has lost. A side with no
  legal move has lost (this can only happen with 4+ stones, since 3 stones can always
  jump somewhere as long as any point is empty, which is guaranteed since at most 18 of
  24 points can be occupied).

## 7. Manifest

`manifest.json` alongside the `.bin` files:
```json
{
  "entries": [
    {"w": 3, "b": 3, "size": 210140, "xxh3": "<16 hex chars>", "solved_at_unix": 1700000000},
    ...
  ]
}
```
`size` is the number of `u16` entries (file size in bytes = `size * 2`). `xxh3` is the
XXH3-64 hash (hex-encoded, lowercase) of the raw file bytes — verify it before trusting
a file. A subspace is present iff both its file exists with the manifest-declared size
*and* its checksum matches; the solve is only complete once all 49 `(w,b)` combinations
for `w,b` in `3..=9` are present.
