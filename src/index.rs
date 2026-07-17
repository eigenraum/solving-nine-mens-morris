//! Subspace indexing: a near-perfect hash from positions to dense array
//! slots, per material subspace `(w, b)`.
//!
//! Scheme (design.md §3):
//! 1. Canonicalize the position (§ symmetry) — this picks, among the 16
//!    symmetric images, the one minimizing `white` first and `black` second.
//! 2. Rank the canonical white set among all *canonical* w-subsets (subsets
//!    that are themselves the minimum of their symmetry orbit).
//! 3. Rank the canonical black set as a `b`-subset of the 24-w points not
//!    occupied by white, using the combinatorial number system.
//! 4. `index = white_rank * C(24-w, b) + black_rank`.
//!
//! **Wasted slots.** When the canonical white set has a nontrivial
//! symmetry stabilizer, several raw `(white, black)` pairs share the same
//! white set but are *not* related by any stabilizer element to the
//! minimal black configuration; each occupies its own slot in step 3's
//! range, but only one slot per stabilizer-orbit is ever the true image of
//! `canonicalize()`. Those other slots are permanently unused: `index()`
//! never routes an update to them (it always canonicalizes first), so they
//! must be skipped by any code that iterates "all slots" (retrograde
//! init/propagation) — see [`is_canonical_slot`]. This trades a small
//! amount of slack (matching Gasser's own 7.67e9-states-in-9.07e9-slots
//! design) for a much simpler indexing scheme than a truly perfect hash.

use crate::pos::{bits, Position, FULL_MASK};
use crate::symmetry::{self, canonicalize};
use std::sync::OnceLock;

pub const MIN_STONES: usize = 3;
pub const MAX_STONES: usize = 9;
const MAX_N: usize = 25;

/// Pascal's triangle up to `C(24, k)`.
pub static BINOM: [[u64; MAX_N]; MAX_N] = build_binom();

const fn build_binom() -> [[u64; MAX_N]; MAX_N] {
    let mut c = [[0u64; MAX_N]; MAX_N];
    let mut n = 0;
    while n < MAX_N {
        c[n][0] = 1;
        let mut k = 1;
        while k <= n {
            let a = c[n - 1][k - 1];
            let b = if k < n { c[n - 1][k] } else { 0 };
            c[n][k] = a + b;
            k += 1;
        }
        n += 1;
    }
    c
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubspaceId {
    /// Stones of the side to move.
    pub w: u8,
    /// Stones of the opponent.
    pub b: u8,
}

impl SubspaceId {
    pub fn new(w: usize, b: usize) -> Self {
        debug_assert!((MIN_STONES..=MAX_STONES).contains(&w));
        debug_assert!((MIN_STONES..=MAX_STONES).contains(&b));
        SubspaceId {
            w: w as u8,
            b: b as u8,
        }
    }

    pub fn flip(self) -> Self {
        SubspaceId {
            w: self.b,
            b: self.w,
        }
    }
}

struct IndexTables {
    /// `white_sets[w - MIN_STONES]` = sorted canonical w-subsets.
    white_sets: [Vec<u32>; MAX_STONES - MIN_STONES + 1],
}

static TABLES: OnceLock<IndexTables> = OnceLock::new();

fn tables() -> &'static IndexTables {
    TABLES.get_or_init(build_index_tables)
}

fn build_index_tables() -> IndexTables {
    let mut white_sets: [Vec<u32>; MAX_STONES - MIN_STONES + 1] = Default::default();
    for mask in 0u32..(1 << 24) {
        let w = mask.count_ones() as usize;
        if !(MIN_STONES..=MAX_STONES).contains(&w) {
            continue;
        }
        if symmetry::canonicalize_set(mask) == mask {
            white_sets[w - MIN_STONES].push(mask);
        }
    }
    IndexTables { white_sets }
}

/// Number of canonical white-stone configurations for `w` stones.
pub fn n_canonical_white(w: usize) -> usize {
    tables().white_sets[w - MIN_STONES].len()
}

/// Total number of index slots in subspace `(w, b)` (includes wasted
/// slots from nontrivial stabilizers).
pub fn subspace_size(sub: SubspaceId) -> u64 {
    n_canonical_white(sub.w as usize) as u64 * BINOM[24 - sub.w as usize][sub.b as usize]
}

/// Rank a `k`-subset of a `universe`-sized ground set (points renumbered
/// densely in ascending order) using the combinatorial number system.
fn mask_rank(sub: u32, universe: u32) -> u64 {
    debug_assert_eq!(sub & !universe, 0);
    let mut r = 0u64;
    let mut compact_idx = 0usize;
    let mut j = 0usize;
    let mut u = universe;
    while u != 0 {
        let p = u.trailing_zeros() as usize;
        u &= u - 1;
        if sub & (1 << p) != 0 {
            j += 1;
            r += BINOM[compact_idx][j];
        }
        compact_idx += 1;
    }
    r
}

/// Inverse of [`mask_rank`]: reconstruct the `k`-subset of `universe` with
/// the given rank.
fn mask_unrank(mut r: u64, k: usize, universe: u32) -> u32 {
    let avail: Vec<usize> = bits(universe).collect();
    let m = avail.len();
    let mut compact = Vec::with_capacity(k);
    let mut upper = m; // next chosen compact index must be < upper
    let mut j = k;
    while j >= 1 {
        let mut cand = upper - 1;
        while BINOM[cand][j] > r {
            cand -= 1;
        }
        compact.push(cand);
        r -= BINOM[cand][j];
        upper = cand;
        j -= 1;
    }
    compact.reverse();
    let mut mask = 0u32;
    for c in compact {
        mask |= 1 << avail[c];
    }
    mask
}

/// Map a position to its subspace and dense index slot. Always
/// canonicalizes first, so `index` is invariant under all 16 symmetries
/// and always lands on a canonical slot (see module docs).
pub fn index(pos: Position) -> (SubspaceId, u64) {
    let (canon, _) = canonicalize(pos);
    let w = canon.white_count() as usize;
    let b = canon.black_count() as usize;
    let tbl = tables();
    let white_rank = tbl.white_sets[w - MIN_STONES]
        .binary_search(&canon.white())
        .expect("canonicalize() must produce a canonical (minimal-orbit) white set") as u64;
    let universe = FULL_MASK & !canon.white();
    let black_rank = mask_rank(canon.black(), universe);
    let c_universe = BINOM[24 - w][b];
    (SubspaceId::new(w, b), white_rank * c_universe + black_rank)
}

/// Reconstruct *a* position occupying slot `idx` of subspace `sub`. The
/// white set is always canonical, but the black set need not be — the
/// result may not be a canonical position (see [`is_canonical_slot`]).
/// Still always round-trips: `index(unindex(sub, idx)).1` differs from
/// `idx` only when the slot is non-canonical (wasted).
pub fn unindex(sub: SubspaceId, idx: u64) -> Position {
    let (w, b) = (sub.w as usize, sub.b as usize);
    let c_universe = BINOM[24 - w][b];
    let white_rank = (idx / c_universe) as usize;
    let black_rank = idx % c_universe;
    let white = tables().white_sets[w - MIN_STONES][white_rank];
    let universe = FULL_MASK & !white;
    let black = mask_unrank(black_rank, b, universe);
    Position::new(white, black)
}

/// True iff slot `idx` of subspace `sub` holds a position that is its own
/// canonical form — i.e. a "real" (non-wasted) slot. Retrograde
/// initialization and propagation must skip non-canonical slots: every
/// legitimate update to a position's value is routed through [`index`],
/// which always resolves to the canonical slot, so a non-canonical slot's
/// own entry is never read by anything and must not be independently
/// processed (doing so would double-count graph edges through its
/// symmetric twin).
pub fn is_canonical_slot(sub: SubspaceId, idx: u64) -> bool {
    symmetry::is_canonical(unindex(sub, idx))
}

/// All 49 ordered subspaces, `(w, b)` for `w, b` in `[3, 9]`.
pub fn all_subspaces() -> Vec<SubspaceId> {
    let mut v = Vec::new();
    for w in MIN_STONES..=MAX_STONES {
        for b in MIN_STONES..=MAX_STONES {
            v.push(SubspaceId::new(w, b));
        }
    }
    v
}

/// The 28 unordered material pairs `{a, b}`, in ascending total-stone
/// order (Gasser's Figure 4 dependency DAG) — each pair's states only
/// have capture-successors in pairs earlier in this order.
pub fn solve_order() -> Vec<(usize, usize)> {
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for a in MIN_STONES..=MAX_STONES {
        for b in a..=MAX_STONES {
            pairs.push((a, b));
        }
    }
    pairs.sort_by_key(|&(a, b)| (a + b, a));
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn binom_matches_known_values() {
        assert_eq!(BINOM[24][9], 1_307_504);
        assert_eq!(BINOM[24][3], 2024);
        assert_eq!(BINOM[5][0], 1);
        assert_eq!(BINOM[5][5], 1);
    }

    #[test]
    fn mask_rank_unrank_bijection_exhaustive_small() {
        // universe of 8 points, k=3: exhaustively check all C(8,3)=56 subsets
        let universe: u32 = 0xFF;
        let k = 3;
        let mut seen = std::collections::HashSet::new();
        for mask in 0u32..256 {
            if mask.count_ones() as usize != k {
                continue;
            }
            let r = mask_rank(mask, universe);
            assert!(seen.insert(r), "duplicate rank {r}");
            let back = mask_unrank(r, k, universe);
            assert_eq!(back, mask, "unrank({r}) != {mask:b}");
        }
        assert_eq!(seen.len(), BINOM[8][3] as usize);
    }

    #[test]
    fn mask_rank_handles_non_trivial_universe() {
        // universe = points {2,5,7,10,15,20} (6 points), k=2
        let pts = [2u32, 5, 7, 10, 15, 20];
        let universe: u32 = pts.iter().map(|p| 1 << p).sum();
        let k = 2;
        let mut seen = std::collections::HashSet::new();
        for i in 0..pts.len() {
            for j in (i + 1)..pts.len() {
                let mask = (1 << pts[i]) | (1 << pts[j]);
                let r = mask_rank(mask, universe);
                assert!(seen.insert(r));
                assert_eq!(mask_unrank(r, k, universe), mask);
            }
        }
        assert_eq!(seen.len(), BINOM[6][2] as usize);
    }

    #[test]
    fn subspace_sizes_sum_matches_expected_order_of_magnitude() {
        let total: u64 = all_subspaces().iter().map(|&s| subspace_size(s)).sum();
        // design.md computed ~8.9e9 after 16-fold symmetry (upper bound
        // including wasted slots); sanity-check order of magnitude.
        assert!(total > 7_000_000_000);
        assert!(total < 10_000_000_000);
    }

    #[test]
    fn solve_order_is_ascending_by_total_stones() {
        let order = solve_order();
        assert_eq!(order.len(), 28);
        let mut last_total = 0;
        for (a, b) in order {
            assert!(a <= b);
            assert!(a + b >= last_total);
            last_total = a + b;
        }
    }

    #[test]
    fn index_is_symmetry_invariant() {
        let white = 0b0000_0000_1110_0111u32;
        let black = 0b0011_0011_0000_1000_0000u32 & !white;
        let pos = Position::new(white, black);
        let (sub0, idx0) = index(pos);
        for s in 0..symmetry::N_SYMS {
            let sym_pos = symmetry::apply_pos(s, pos);
            let (sub, idx) = index(sym_pos);
            assert_eq!(sub, sub0);
            assert_eq!(idx, idx0, "symmetry {s} broke index invariance");
        }
    }

    #[test]
    fn canonical_slots_round_trip_exactly() {
        // For every canonical slot, index(unindex(slot)) must reproduce it
        // exactly (this is the meaningful round-trip guarantee — wasted
        // slots are explicitly exempted, see module docs).
        for &(a, b) in &[(3, 3), (4, 3), (3, 4)] {
            let sub = SubspaceId::new(a, b);
            let size = subspace_size(sub);
            let step = (size / 500).max(1);
            let mut idx = 0u64;
            while idx < size {
                if is_canonical_slot(sub, idx) {
                    let pos = unindex(sub, idx);
                    let (sub2, idx2) = index(pos);
                    assert_eq!(sub2, sub);
                    assert_eq!(idx2, idx, "canonical slot {idx} in {sub:?} did not round-trip");
                }
                idx += step;
            }
        }
    }

    #[test]
    fn non_canonical_slots_exist_and_are_consistent() {
        // The 3-3 subspace's all-corners-white set (a highly symmetric
        // configuration) should have a nontrivial stabilizer and hence at
        // least one wasted slot whose unindexed position is not canonical,
        // yet re-indexing it must still land on a valid, smaller-or-equal
        // canonical slot in the same subspace.
        let sub = SubspaceId::new(3, 3);
        let size = subspace_size(sub);
        let mut found_wasted = false;
        for idx in 0..size.min(20_000) {
            if !is_canonical_slot(sub, idx) {
                found_wasted = true;
                let pos = unindex(sub, idx);
                let (sub2, idx2) = index(pos);
                assert_eq!(sub2, sub);
                assert!(idx2 <= idx);
                assert!(is_canonical_slot(sub2, idx2));
            }
        }
        assert!(found_wasted, "expected at least one wasted slot in 3-3 within the scanned range");
    }

    proptest! {
        #[test]
        fn index_never_panics_on_arbitrary_valid_positions(
            w in 3usize..=9, b in 3usize..=9,
            seed in any::<u64>()
        ) {
            // deterministic pseudo-random disjoint white/black sets of sizes w,b
            let mut pts: Vec<u32> = (0..24).collect();
            // simple LCG-based shuffle from seed
            let mut s = seed | 1;
            for i in (1..pts.len()).rev() {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                let j = (s >> 33) as usize % (i + 1);
                pts.swap(i, j);
            }
            if w + b > 24 { return Ok(()); }
            let white: u32 = pts[..w].iter().map(|&p| 1 << p).sum();
            let black: u32 = pts[w..w + b].iter().map(|&p| 1 << p).sum();
            let pos = Position::new(white, black);
            let (sub, idx) = index(pos);
            prop_assert_eq!(sub, SubspaceId::new(w, b));
            prop_assert!(idx < subspace_size(sub));
        }
    }
}
