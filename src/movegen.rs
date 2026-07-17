//! Move generation: forward moves (placement and movement phases, with
//! mill-closure captures) and reverse quiet-move generation (used by
//! retrograde analysis to find within-pair predecessors).

use crate::board::ADJ;
use crate::pos::{bits, Position, FULL_MASK};

/// Rule decision (fixed to match Gasser 1996): closing two mills with one
/// move still removes only one stone; if all opponent stones are in mills,
/// a mill closure may remove any of them.
///
/// Returns, for a stone just placed/moved to `dest` by `mover`, the set of
/// resulting opponent bitboards — one per legal capture choice, or a single
/// unchanged opponent bitboard if no mill was closed.
fn resolve_mill(mover: u32, opponent: u32, dest: usize) -> Vec<u32> {
    if !Position::is_mill_at(mover, dest) {
        return vec![opponent];
    }
    let removable = removable_stones(opponent);
    bits(removable).map(|s| opponent & !(1 << s)).collect()
}

/// Opponent stones that may legally be captured: those not part of any of
/// the opponent's own mills, or — if every opponent stone is in a mill —
/// all of them.
fn removable_stones(opponent: u32) -> u32 {
    let mut not_in_mill = 0u32;
    for p in bits(opponent) {
        if !Position::is_mill_at(opponent, p) {
            not_in_mill |= 1 << p;
        }
    }
    if not_in_mill == 0 {
        opponent
    } else {
        not_in_mill
    }
}

/// Forward moves during the placement (opening) phase: place a mover stone
/// on any empty point, resolving mill captures. Returned positions are
/// perspective-flipped (opponent to move next).
pub fn moves_placement(mover: u32, opponent: u32) -> Vec<Position> {
    let empty = !(mover | opponent) & FULL_MASK;
    let mut out = Vec::new();
    for to in bits(empty) {
        let new_mover = mover | (1 << to);
        for new_opp in resolve_mill(new_mover, opponent, to) {
            out.push(Position::new(new_opp, new_mover));
        }
    }
    out
}

/// Forward moves during the movement/endgame phase: slide to an adjacent
/// empty point, or (with exactly 3 stones) jump to any empty point.
/// Resolves mill captures. Returned positions are perspective-flipped.
pub fn moves_movement(mover: u32, opponent: u32) -> Vec<Position> {
    let empty = !(mover | opponent) & FULL_MASK;
    let jump = mover.count_ones() == 3;
    let mut out = Vec::new();
    for from in bits(mover) {
        let dests = if jump { empty } else { ADJ[from] & empty };
        for to in bits(dests) {
            let new_mover = (mover & !(1 << from)) | (1 << to);
            for new_opp in resolve_mill(new_mover, opponent, to) {
                out.push(Position::new(new_opp, new_mover));
            }
        }
    }
    out
}

/// All legal successors of `pos` in the movement phase (convenience
/// wrapper). Does not check terminal conditions.
pub fn successors(pos: Position) -> Vec<Position> {
    moves_movement(pos.white(), pos.black())
}

/// Reverse *quiet* moves: given the current (post-move) position `pos`
/// (white to move), enumerate predecessor positions `p'` (also white to
/// move, in `p'`'s own frame) that have `pos` as a successor via a quiet
/// (non-capturing) slide or jump. Both positions have identical stone
/// counts on both sides — this generator never crosses subspace pairs.
///
/// Derivation: if predecessor mover `A` moves a stone `from -> to`
/// (quietly) while opponent `B` is untouched, the resulting state (from
/// `B`'s perspective, now to move) is `Position::new(B, A')` where `A'` is
/// `A` with the stone relocated. So given `pos = Position::new(w, b)`,
/// we have `B = w` (unchanged) and `A' = b`; we invert the move on `b` to
/// recover `A`, and require it to be quiet from `A`'s side.
pub fn quiet_predecessors(pos: Position) -> Vec<Position> {
    let a_prime = pos.black();
    let b = pos.white();
    let occupied = a_prime | b;
    let jump = a_prime.count_ones() == 3;
    let mut out = Vec::new();
    for to in bits(a_prime) {
        // Whether the stone that just arrived at `to` closed a mill depends
        // only on the *final* board `a_prime` (it's the only stone that
        // changed) — not on where it came from. If it did, this arrival was
        // a capturing move (a different, cross-pair transition), so no
        // predecessor reached via `to` can be a quiet predecessor of `pos`.
        if Position::is_mill_at(a_prime, to) {
            continue;
        }
        let candidates = if jump {
            !occupied & FULL_MASK
        } else {
            ADJ[to] & !occupied & FULL_MASK
        };
        for from in bits(candidates) {
            let a = (a_prime & !(1 << to)) | (1 << from);
            out.push(Position::new(a, b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    fn random_position(white_n: usize, black_n: usize) -> impl Strategy<Value = Position> {
        (Just(white_n), Just(black_n)).prop_flat_map(|(w, b)| {
            proptest::sample::subsequence((0..24).collect::<Vec<_>>(), w + b).prop_map(
                move |mut pts| {
                    pts.sort();
                    // simple deterministic-ish split based on parity to get variety
                    let white: u32 = pts[..w].iter().map(|p| 1u32 << p).sum();
                    let black: u32 = pts[w..w + b].iter().map(|p| 1u32 << p).sum();
                    Position::new(white, black)
                },
            )
        })
    }

    #[test]
    fn known_blocked_position_has_no_moves() {
        let white = 1u32;
        let black = (1 << 1) | (1 << 7);
        let succ = moves_movement(white, black);
        assert!(succ.is_empty());
    }

    #[test]
    fn placement_move_count_matches_empty_squares_when_no_mills() {
        let white = 0u32;
        let black = 0u32;
        let succ = moves_placement(white, black);
        assert_eq!(succ.len(), 24); // empty board, no mills possible
    }

    #[test]
    fn mill_closure_offers_one_successor_per_removable_stone() {
        // white completes mill a7-d7-g7 (points 0,1,2); black has 3 stones,
        // none of which are in a mill together, so all 3 are removable.
        let white_after = 0b111u32; // points 0,1,2
        let black = (1 << 5) | (1 << 10) | (1 << 15);
        let results = resolve_mill(white_after, black, 2);
        assert_eq!(results.len(), black.count_ones() as usize);
    }

    #[test]
    fn mill_closure_with_all_opponent_in_mills_can_remove_any() {
        let white_dest_mover = 0b111u32; // completed mill at 0,1,2
        let black = crate::board::MILLS[3] & FULL_MASK; // an actual opponent mill
        let results = resolve_mill(white_dest_mover, black, 2);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn no_mill_no_capture_single_result() {
        let mover = 0b1u32;
        let opp = 0b10u32;
        let results = resolve_mill(mover, opp, 0);
        assert_eq!(results, vec![opp]);
    }

    proptest! {
        #[test]
        fn quiet_predecessor_roundtrip(
            pos in random_position(6, 6)
        ) {
            // For every quiet successor of `pos`, `pos` must appear in the
            // successor's quiet_predecessors set.
            // A successor is quiet iff the opponent's bitboard is untouched
            // (mill closures always remove a stone, so equality here proves
            // no capture happened).
            let quiet_succs: Vec<Position> = moves_movement(pos.white(), pos.black())
                .into_iter()
                .filter(|s| s.white() == pos.black())
                .collect();
            for s in quiet_succs {
                let preds: HashSet<Position> = quiet_predecessors(s).into_iter().collect();
                prop_assert!(preds.contains(&pos), "pos {:?} missing from predecessors of {:?}", pos, s);
            }
        }
    }

    #[test]
    fn quiet_predecessors_excludes_mill_closing_arrival() {
        // succ.black() = {0,1,2}: a completed mill. Any hypothesis "the
        // stone at `to`=2 just arrived from an adjacent empty point" would
        // mean that arrival closed the 0-1-2 mill — a capturing move, not a
        // quiet one — so it must not be offered as a quiet predecessor, even
        // though a naive "undo the slide" reconstruction looks plausible
        // (point 2's neighbors are 1 and 3; 3 is empty here).
        let succ = Position::new(/* white (opponent, unchanged) */ 1 << 10, /* black (mover-after) */ 0b111);
        let preds = quiet_predecessors(succ);
        for p in &preds {
            // No predecessor may claim the mover arrived at 2 via 3 while
            // already holding 0 and 1 (that reconstructs the illegal case).
            assert!(
                !(p.white() & (1 << 0) != 0 && p.white() & (1 << 1) != 0 && p.white() & (1 << 3) != 0),
                "predecessor {:?} reconstructs a mill-closing arrival at point 2",
                p
            );
        }
        // Sanity: without the gate, this candidate (from=3) would otherwise
        // have been generated, since 3 is adjacent to 2 and empty.
        let illegal_predecessor = Position::new(0b1011, 1 << 10); // white={0,1,3}, black unchanged
        assert!(!preds.contains(&illegal_predecessor));
    }

    #[test]
    fn quiet_predecessors_includes_genuinely_quiet_reversal() {
        // succ.black() = {0,1,4}: no mill through point 4 alone (mill needs
        // 3,4 or 4,5,6 style with matching third point absent here), so a
        // stone arriving at 4 from adjacent empty point 3 is a legitimate
        // quiet predecessor.
        let succ = Position::new(1 << 10, 0b10011); // white unchanged, black={0,1,4}
        let expected = Position::new(0b1011, 1 << 10); // white={0,1,3}, black unchanged
        let preds = quiet_predecessors(succ);
        assert!(preds.contains(&expected));
    }
}
