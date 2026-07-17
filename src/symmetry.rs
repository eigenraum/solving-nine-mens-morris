//! Board symmetries and canonicalization.
//!
//! The mill graph has 16 automorphisms: the dihedral group of the square
//! (rotations/reflections acting on the 8 positions within each ring) times
//! swapping the outer and inner rings (the middle ring is fixed, since its
//! points have degree 4 while outer/inner midpoints have degree 3 — only
//! outer<->inner is a valid swap).
//!
//! Point `p = ring*8 + i` maps under symmetry `(a, b, s)` to
//! `ring' * 8 + i'` where `i' = (a*i + b) mod 8` and `ring' = s ? 2-ring :
//! ring`, for `a in {1, -1}`, `b in {0, 2, 4, 6}`, `s in {0, 1}` — 2*4*2 =
//! 16 combinations. `a=-1` gives a reflection, `b` gives a rotation by
//! `b/2` quarter-turns (since ring index step 2 = one board position).

use crate::board::N;
use crate::pos::Position;

pub const N_SYMS: usize = 16;

/// `PERMS[s][p]` = the point that `p` maps to under symmetry `s`.
pub static PERMS: [[u8; N]; N_SYMS] = build_perms();

const fn sym_map(a: i32, b: i32, s: i32, p: usize) -> usize {
    let ring = p / 8;
    let i = (p % 8) as i32;
    let mut i2 = (a * i + b) % 8;
    if i2 < 0 {
        i2 += 8;
    }
    let ring2 = if s == 1 { 2 - ring } else { ring };
    ring2 * 8 + i2 as usize
}

const fn build_perms() -> [[u8; N]; N_SYMS] {
    let mut perms = [[0u8; N]; N_SYMS];
    let a_vals = [1i32, -1i32];
    let b_vals = [0i32, 2, 4, 6];
    let s_vals = [0i32, 1];
    let mut idx = 0;
    let mut ai = 0;
    while ai < 2 {
        let mut bi = 0;
        while bi < 4 {
            let mut si = 0;
            while si < 2 {
                let a = a_vals[ai];
                let b = b_vals[bi];
                let s = s_vals[si];
                let mut p = 0;
                while p < N {
                    perms[idx][p] = sym_map(a, b, s, p) as u8;
                    p += 1;
                }
                idx += 1;
                si += 1;
            }
            bi += 1;
        }
        ai += 1;
    }
    perms
}

/// Apply symmetry `s` to a 24-bit point set.
#[inline]
pub fn apply(sym: usize, mask: u32) -> u32 {
    let perm = &PERMS[sym];
    let mut out = 0u32;
    let mut m = mask;
    while m != 0 {
        let p = m.trailing_zeros() as usize;
        m &= m - 1;
        out |= 1 << perm[p];
    }
    out
}

/// Apply symmetry `s` to a position (both colors, side-to-move unchanged).
#[inline]
pub fn apply_pos(sym: usize, pos: Position) -> Position {
    Position::new(apply(sym, pos.white()), apply(sym, pos.black()))
}

/// The canonical form of a position: over all 16 symmetries, the image
/// that minimizes `white` first, breaking ties by minimizing `black`
/// (i.e. lexicographic on `(white, black)` with white primary — matching
/// the indexing scheme, which ranks the canonical white set first). Also
/// returns the symmetry index that produced it (the first one found, if
/// the stabilizer is nontrivial).
///
/// Note this is *not* the same as comparing `pos.0` as a raw `u64`: that
/// packs black into the high bits, which would make black the primary
/// sort key instead.
pub fn canonicalize(pos: Position) -> (Position, usize) {
    let mut best = apply_pos(0, pos);
    let mut best_key = (best.white(), best.black());
    let mut best_sym = 0;
    for s in 1..N_SYMS {
        let cand = apply_pos(s, pos);
        let key = (cand.white(), cand.black());
        if key < best_key {
            best = cand;
            best_key = key;
            best_sym = s;
        }
    }
    (best, best_sym)
}

/// True iff `pos` is already its own canonical form — i.e. no symmetry
/// produces a lexicographically smaller `(white, black)` image. Positions
/// whose canonical white set has a nontrivial stabilizer have several raw
/// `(white, black)` encodings in the same orbit; exactly one is canonical.
/// This is used to skip non-canonical index slots during retrograde
/// analysis, so that every game-graph edge is counted exactly once.
pub fn is_canonical(pos: Position) -> bool {
    canonicalize(pos).0 == pos
}

/// The canonical form of just a point set (used for the white-set tables in
/// the indexing scheme).
pub fn canonicalize_set(mask: u32) -> u32 {
    let mut best = mask;
    for s in 1..N_SYMS {
        let cand = apply(s, mask);
        if cand < best {
            best = cand;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{ADJ, MILLS};
    use proptest::prelude::*;
    use std::collections::HashSet;

    #[test]
    fn all_perms_are_bijections() {
        for s in 0..N_SYMS {
            let mut seen = [false; N];
            for p in 0..N {
                let q = PERMS[s][p] as usize;
                assert!(!seen[q], "sym {s} not injective at {q}");
                seen[q] = true;
            }
        }
    }

    #[test]
    fn all_16_perms_distinct() {
        let set: HashSet<[u8; N]> = PERMS.iter().cloned().collect();
        assert_eq!(set.len(), N_SYMS);
    }

    #[test]
    fn identity_present() {
        let id: [u8; N] = std::array::from_fn(|i| i as u8);
        assert!(PERMS.contains(&id));
    }

    #[test]
    fn every_perm_has_inverse_in_set() {
        for s in 0..N_SYMS {
            let mut inv = [0u8; N];
            for p in 0..N {
                inv[PERMS[s][p] as usize] = p as u8;
            }
            assert!(PERMS.contains(&inv), "sym {s} has no inverse in the set");
        }
    }

    #[test]
    fn group_closed_under_composition() {
        // composing any two of the 16 permutations must land back in the set
        for s1 in 0..N_SYMS {
            for s2 in 0..N_SYMS {
                let mut comp = [0u8; N];
                for p in 0..N {
                    comp[p] = PERMS[s2][PERMS[s1][p] as usize];
                }
                assert!(
                    PERMS.contains(&comp),
                    "composition of sym {s1} and {s2} not in group"
                );
            }
        }
    }

    #[test]
    fn perms_are_adjacency_automorphisms() {
        for s in 0..N_SYMS {
            for p in 0..N {
                let mapped_adj = apply(s, ADJ[p]);
                let q = PERMS[s][p] as usize;
                assert_eq!(
                    mapped_adj, ADJ[q],
                    "sym {s} does not map ADJ[{p}] onto ADJ[{q}]"
                );
            }
        }
    }

    #[test]
    fn perms_are_mill_automorphisms() {
        for s in 0..N_SYMS {
            let mapped_mills: HashSet<u32> = MILLS.iter().map(|&m| apply(s, m)).collect();
            let orig_mills: HashSet<u32> = MILLS.iter().cloned().collect();
            assert_eq!(mapped_mills, orig_mills, "sym {s} does not preserve mill set");
        }
    }

    fn random_mask() -> impl Strategy<Value = u32> {
        any::<u32>().prop_map(|x| x & 0x00FF_FFFF)
    }

    proptest! {
        // The whole indexing scheme (index.rs) relies on canonicalize being
        // idempotent: re-canonicalizing an already-canonical position must
        // be a no-op. Otherwise "canonical slots" wouldn't be well-defined.
        #[test]
        fn canonicalize_is_idempotent(white in random_mask(), black_seed in random_mask()) {
            let black = black_seed & !white;
            let pos = Position::new(white, black);
            let (canon, _) = canonicalize(pos);
            prop_assert!(is_canonical(canon));
            let (canon2, _) = canonicalize(canon);
            prop_assert_eq!(canon2, canon);
        }

        #[test]
        fn canonical_form_invariant_under_symmetry(mask in random_mask(), sym in 0..N_SYMS) {
            let c1 = canonicalize_set(mask);
            let c2 = canonicalize_set(apply(sym, mask));
            prop_assert_eq!(c1, c2);
        }

        #[test]
        fn canonicalize_pos_invariant_under_symmetry(
            white in random_mask(), black_seed in random_mask(), sym in 0..N_SYMS
        ) {
            let black = black_seed & !white; // keep disjoint
            let pos = Position::new(white, black);
            let (c1, _) = canonicalize(pos);
            let (c2, _) = canonicalize(apply_pos(sym, pos));
            prop_assert_eq!(c1, c2);
        }

        #[test]
        fn apply_pos_preserves_stone_counts(white in random_mask(), black_seed in random_mask(), sym in 0..N_SYMS) {
            let black = black_seed & !white;
            let pos = Position::new(white, black);
            let out = apply_pos(sym, pos);
            prop_assert_eq!(out.white_count(), pos.white_count());
            prop_assert_eq!(out.black_count(), pos.black_count());
        }
    }
}
