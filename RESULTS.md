# Results

## Mid/endgame database: complete and exhaustively verified

All 28 unordered material pairs (49 ordered subspaces, every combination of 3–9 stones
per side) have been solved and **exhaustively verified**: the forward-consistency
checker (`ninemm verify`) recomputed every single stored value directly from its
successors and compared it against what's on disk, for all ~9.16 billion canonical
states (raw, unreduced count: 9,193,626,407 index slots across all 49 subspaces,
including symmetry-stabilizer padding — see `design.md` §3). **Zero mismatches.**

This exhaustive result reflects the *second* full solve. The first full run had a real
bug (see git history around "Fix a real double-processing bug in retro.rs"): a subtle
double-counting error in the retrograde propagation logic that only manifested at
scale, first caught by `verify` on the `{4,6}` pair. Everything from `{4,6}` onward (21
of 28 pairs) was deleted and re-solved after the fix; the re-solved database passed
full verification cleanly, with the numbers below.

### Aggregate statistics (canonical states only — see `design.md` §3 for why "wasted"
### index slots aren't counted)

| | count | % |
|---|---:|---:|
| Total canonical states | 8,904,593,601 | 100% |
| Wins (side to move) | 4,325,452,782 | 48.58% |
| Losses (side to move) | 2,747,046,939 | 30.85% |
| Draws | 1,832,093,880 | 20.57% |

- **Deepest win**: 203 plies to conversion/terminal.
- **Deepest loss**: 204 plies to conversion/terminal.

(Depths here are DTC-style — plies until a capture converts to a smaller,
already-solved pair, or a true terminal — see `design.md` §4. They are not directly
comparable to Gasser's "mill closure in 187 plies" figure, which measures a different
thing across the whole midgame/endgame rather than within one material pair; both are
consistent with "very long forced sequences exist" being a real feature of this game.)

### Per-subspace table

`w`/`b` = stones for the side to move / opponent. Depths are plies to conversion or
terminal within that subspace (0 = immediate terminal).

| w-b | wins | losses | draws | max win depth | max loss depth |
|---|---:|---:|---:|---:|---:|
| 3-3 | 140,621 | 28,736 | 269 | 25 | 26 |
| 3-4 | 102,281 | 0 | 658,117 | 33 | 0 |
| 4-3 | 75,397 | 3,095 | 681,906 | 1 | 32 |
| 3-5 | 6,301 | 9,677 | 2,564,412 | 31 | 2 |
| 5-3 | 580,660 | 0 | 1,999,730 | 3 | 0 |
| 4-4 | 159 | 29 | 3,225,409 | 9 | 8 |
| 3-6 | 0 | 192,000 | 6,683,320 | 0 | 6 |
| 6-3 | 2,752,371 | 0 | 4,122,949 | 7 | 0 |
| 4-5 | 51 | 1,510 | 10,308,935 | 5 | 28 |
| 5-4 | 9,889 | 8 | 10,300,599 | 29 | 4 |
| 3-7 | 0 | 6,965,784 | 7,759,904 | 0 | 30 |
| 7-3 | 13,411,581 | 0 | 1,314,107 | 31 | 0 |
| 4-6 | 22 | 1,649,972 | 24,115,798 | 3 | 156 |
| 6-4 | 5,985,293 | 4 | 19,780,495 | 157 | 2 |
| 5-5 | 28,819 | 4,289 | 30,881,316 | 57 | 56 |
| 3-8 | 0 | 23,397,858 | 2,365,978 | 0 | 34 |
| 8-3 | 25,697,503 | 0 | 66,333 | 33 | 0 |
| 4-7 | 18 | 37,626,914 | 13,889,596 | 3 | 112 |
| 7-4 | 47,531,650 | 3 | 3,984,875 | 111 | 2 |
| 5-6 | 18,321 | 4,806,956 | 67,290,795 | 53 | 162 |
| 6-5 | 17,229,622 | 2,052 | 54,884,398 | 163 | 48 |
| 3-9 | 0 | 37,197,445 | 11,783 | 0 | 30 |
| 9-3 | 37,209,196 | 0 | 32 | 25 | 0 |
| 4-8 | 15 | 77,883,072 | 5,822,711 | 3 | 112 |
| 8-4 | 83,063,749 | 3 | 642,046 | 111 | 2 |
| 5-7 | 12,156 | 80,867,010 | 53,031,730 | 51 | 160 |
| 7-5 | 117,737,860 | 1,393 | 16,171,643 | 159 | 50 |
| 6-6 | 23,250,808 | 5,273,162 | 127,705,390 | 167 | 166 |
| 4-9 | 23 | 111,463,785 | 133,804 | 3 | 110 |
| 9-4 | 111,596,447 | 4 | 1,161 | 103 | 2 |
| 5-8 | 9,430 | 175,925,821 | 24,917,737 | 33 | 160 |
| 8-5 | 197,131,113 | 1,000 | 3,720,875 | 153 | 20 |
| 6-7 | 19,007,081 | 99,105,562 | 149,680,645 | 171 | 184 |
| 7-6 | 197,340,770 | 3,342,246 | 67,110,272 | 185 | 170 |
| 5-9 | 2,872 | 241,700,295 | 3,776,081 | 21 | 114 |
| 9-5 | 245,214,294 | 191 | 264,763 | 113 | 20 |
| 6-8 | 9,014,966 | 259,100,308 | 100,090,242 | 171 | 186 |
| 8-6 | 344,867,329 | 1,152,604 | 22,185,583 | 185 | 170 |
| 7-7 | 197,782,562 | 63,016,948 | 159,993,586 | 181 | 180 |
| 6-9 | 2,904,209 | 382,611,312 | 23,591,219 | 173 | 170 |
| 9-6 | 406,106,715 | 276,492 | 2,723,533 | 167 | 172 |
| 7-8 | 111,934,094 | 221,367,411 | 192,677,895 | 183 | 204 |
| 8-7 | 420,563,898 | 22,670,392 | 82,745,110 | 203 | 182 |
| 7-9 | 41,317,865 | 401,584,464 | 83,077,071 | 185 | 202 |
| 9-7 | 502,466,519 | 5,852,417 | 17,660,464 | 201 | 184 |
| 8-8 | 318,690,930 | 107,554,848 | 165,480,912 | 195 | 196 |
| 8-9 | 149,356,285 | 247,948,155 | 128,674,960 | 189 | 202 |
| 9-8 | 434,874,914 | 34,681,537 | 56,422,949 | 201 | 188 |
| 9-9 | 240,426,123 | 91,780,175 | 76,900,442 | 191 | 190 |

Reproduce with `./target/release/ninemm db-stats --dir db` after a full solve.

## Opening (18-ply placement phase): in progress

The empty-board game-theoretic value — Nine Men's Morris' headline result, matching
Gasser's published conclusion that the game is a **draw** with perfect play — is
computed by an alpha-beta search over the full database (`ninemm::opening`). This
search is memory-intensive (it can, in principle, reach almost any material split, so
it needs access to the full ~17 GB database) and was still running at the time this
file was last updated, constrained by this machine's 18 GB of RAM (the search process
was observed spending significant time in disk I/O waiting on memory-mapped pages,
rather than being compute-bound).

This does not affect the mid/endgame result above, which is independently complete and
verified. On a machine with more headroom over the database's ~17 GB footprint, the
opening search should complete comfortably (Gasser's own equivalent search visited only
a few tens of thousands of nodes at the 8-ply level with move ordering and a
transposition table). This section will be updated with the confirmed root value once
available.
