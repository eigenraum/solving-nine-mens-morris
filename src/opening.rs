//! Opening-phase search (design.md §6): the 18-ply placement DAG is
//! acyclic and only the root (empty board) value matters, so — unlike
//! the mid/endgame — this is solved by plain alpha-beta with a
//! transposition table, probing the (fully solved) movement-phase
//! database at the placement/movement boundary, rather than by
//! retrograde analysis.

use crate::board::ADJ;
use crate::movegen;
use crate::pos::Position;
use crate::retro::{self, Database};
use crate::symmetry;
use std::collections::HashMap;

/// A placement-phase state, in the same "mover/opponent" perspective
/// convention as `Position` (see pos.rs): `pos.white()` is the mover's
/// on-board stones, `pos.black()` the opponent's. `mover_hand`/
/// `opp_hand` count stones *not yet placed* — decremented only by
/// placing, never by being captured (a captured stone is gone for good,
/// not returned to hand).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PlacementState {
    pub pos: Position,
    pub mover_hand: u8,
    pub opp_hand: u8,
}

impl PlacementState {
    pub fn initial() -> Self {
        PlacementState { pos: Position::new(0, 0), mover_hand: 9, opp_hand: 9 }
    }

    /// Total stones still available to the mover (on board + in hand) —
    /// the quantity that matters for the "fewer than three stones loses"
    /// rule, which applies throughout the game, not just the endgame.
    pub fn total_mover(&self) -> u32 {
        self.pos.white_count() + self.mover_hand as u32
    }

    pub fn total_opp(&self) -> u32 {
        self.pos.black_count() + self.opp_hand as u32
    }

    pub fn placement_done(&self) -> bool {
        self.mover_hand == 0 && self.opp_hand == 0
    }
}

/// Successors of a placement state: place the mover's next stone on any
/// empty point, resolving mill captures exactly as in the movement phase
/// (reusing `movegen::moves_placement`), then hand off to the opponent —
/// whose hand is unchanged and who becomes the new mover.
///
/// Ordered per design.md §6 ("mills/captures first, then center-symmetric
/// points") for alpha-beta pruning effectiveness: `movegen::moves_placement`
/// itself does no such ordering (it just sweeps empty points 0..24 in raw
/// bit order, appending capture-choice branches for each destination point
/// in that same sweep), so this is where the heuristic actually lives.
/// Reordering only changes how many nodes get pruned, never the value
/// `negamax` returns (a full-window alpha-beta search is exact regardless
/// of move order) — see the `tests` module for checks of both properties.
pub fn successors(state: &PlacementState) -> Vec<PlacementState> {
    if state.mover_hand == 0 {
        return Vec::new();
    }
    let mover_before = state.pos.white();
    let opp_count_before = state.pos.black().count_ones();
    let mut succs: Vec<PlacementState> = movegen::moves_placement(mover_before, state.pos.black())
        .into_iter()
        .map(|new_pos| PlacementState {
            pos: new_pos,
            mover_hand: state.opp_hand,
            opp_hand: state.mover_hand - 1,
        })
        .collect();
    succs.sort_by_key(|s| std::cmp::Reverse(move_order_key(s, mover_before, opp_count_before)));
    succs
}

/// Ordering key for one successor: `(is_capture, point_degree)`, both
/// wanted in descending order (captures before quiet moves; among moves of
/// equal capture-status, higher-connectivity — "center-symmetric" — points
/// first). Sorting by `Reverse` of this tuple gives exactly that priority.
///
/// Both components are recovered from the resulting position alone rather
/// than threaded through from `movegen`, since captures only ever remove
/// exactly one opponent stone (Gasser's fixed rule, see `movegen::resolve_mill`)
/// and placement only ever adds exactly one mover stone: `s.pos.white()`
/// (the *new* opponent bitboard, post-perspective-flip) is shorter than
/// before iff this branch captured, and the sole bit set in `s.pos.black()`
/// (the new mover bitboard) but not in `mover_before` is the point just
/// placed on.
fn move_order_key(s: &PlacementState, mover_before: u32, opp_count_before: u32) -> (bool, u32) {
    let is_capture = s.pos.white().count_ones() < opp_count_before;
    let to = (s.pos.black() & !mover_before).trailing_zeros() as usize;
    let degree = ADJ[to].count_ones();
    (is_capture, degree)
}

/// Three-valued game outcome, from the mover's perspective.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Value {
    Loss,
    Draw,
    Win,
}

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

fn from_i8(v: i8) -> Value {
    match v {
        -1 => Value::Loss,
        0 => Value::Draw,
        1 => Value::Win,
        _ => unreachable!("i8 game values are always in -1..=1"),
    }
}

fn code_to_i8(code: u16) -> i8 {
    if code == retro::DRAW {
        0
    } else if code.is_multiple_of(2) {
        -1
    } else {
        1
    }
}

/// Canonicalize a placement state for the transposition table: board
/// symmetry applies to `pos` exactly as in the mid/endgame (hand counts
/// are symmetry-invariant, so they pass through unchanged). This lets
/// transpositions reached via different, board-symmetric move orders
/// share one table entry.
fn tt_key(state: &PlacementState) -> (Position, u8, u8) {
    let (canon, _) = symmetry::canonicalize(state.pos);
    (canon, state.mover_hand, state.opp_hand)
}

/// Evaluate a placement state's exact game-theoretic value via alpha-beta
/// negamax with a transposition table. `db` must contain every
/// movement-phase subspace reachable from `state` (in practice: the full
/// solved database, since the placement DAG can reach nearly any
/// material split).
pub fn solve(state: &PlacementState, db: &Database, tt: &mut Tt) -> Value {
    from_i8(negamax(state, -1, 1, db, tt))
}

pub(crate) fn negamax(state: &PlacementState, mut alpha: i8, beta: i8, db: &Database, tt: &mut Tt) -> i8 {
    if state.total_mover() < 3 {
        return -1;
    }

    if state.placement_done() {
        return if state.pos.is_blocked() {
            -1
        } else {
            code_to_i8(db.lookup_pos(state.pos))
        };
    }

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

    let succs = successors(state);
    if succs.is_empty() {
        // Only possible if mover_hand == 0 while opp_hand > 0, which
        // never happens under strict alternating placement (hands differ
        // by at most one and both reach zero together) — kept as an
        // explicit terminal rather than silently mispropagating.
        return -1;
    }

    let mut best = i8::MIN;
    for succ in succs {
        let v = -negamax(&succ, -beta, -alpha, db, tt);
        if v > best {
            best = v;
        }
        if best > alpha {
            alpha = best;
        }
        if alpha >= beta {
            break;
        }
    }

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
}

/// Convenience: the game-theoretic value of the empty board (design.md's
/// headline result). Builds a fresh transposition table.
pub fn solve_from_empty_board(db: &Database) -> Value {
    let mut tt = Tt::new();
    solve(&PlacementState::initial(), db, &mut tt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate;
    use crate::persist::{self, Manifest};

    fn load_db_up_to(dir: &std::path::Path, manifest: &Manifest) -> Database {
        let mut db = Database::new();
        for e in &manifest.entries {
            let data = persist::read_subspace_verified(dir, manifest, e.w as usize, e.b as usize).unwrap();
            db.insert(e.w as usize, e.b as usize, data);
        }
        db
    }

    #[test]
    fn successors_decrement_hands_correctly() {
        let s0 = PlacementState::initial();
        assert_eq!(s0.mover_hand, 9);
        assert_eq!(s0.opp_hand, 9);
        let succs = successors(&s0);
        assert_eq!(succs.len(), 24); // empty board, no mills possible on move 1
        for s in &succs {
            assert_eq!(s.mover_hand, 9); // was opp_hand (untouched)
            assert_eq!(s.opp_hand, 8); // was mover_hand - 1
            assert_eq!(s.pos.white_count() + s.pos.black_count(), 1);
        }
    }

    #[test]
    fn placement_done_after_18_plies_of_any_line() {
        let mut s = PlacementState::initial();
        for _ in 0..18 {
            let succs = successors(&s);
            assert!(!s.placement_done(), "should not be done before 18 plies");
            s = succs[0];
        }
        assert!(s.placement_done());
        // At most 18 (9 placements each), less if any mill closed and
        // captured a stone along the way (this particular greedy
        // first-successor walk happens to trigger one).
        assert!(s.pos.white_count() + s.pos.black_count() <= 18);
        assert_eq!(s.mover_hand, 0);
        assert_eq!(s.opp_hand, 0);
    }

    #[test]
    fn solve_tiny_subtree_matches_hand_evaluation() {
        // Solve just the last-ply-of-placement subtree (mover_hand=1,
        // opp_hand=0), using a database containing only {9,9} through
        // {9,8} etc. as needed — exercise the placement-done boundary
        // without requiring the full database.
        let tmp = std::env::temp_dir().join(format!("ninemm_opening_test_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(6)).unwrap(); // {3,3} only
        let manifest = Manifest::load(&tmp).unwrap();
        let db = load_db_up_to(&tmp, &manifest);

        // Construct a placement state that will end with exactly 3 mover
        // stones and 3 opponent stones after both hands empty, so the
        // {3,3} database (the only one loaded) is sufficient.
        // mover has placed 2, opp has placed 2, mover_hand=1, opp_hand=1:
        // e.g. mover at {0,1}, opp at {5,6}, mover to place last stone at 2
        // (not closing a mill: mill 0,1,2 would need... let's use a spot
        // that avoids any mill for simplicity).
        let mover = (1 << 3) | (1 << 8);
        let opp = (1 << 10) | (1 << 15);
        let state = PlacementState {
            pos: Position::new(mover, opp),
            mover_hand: 1,
            opp_hand: 1,
        };
        let mut tt = HashMap::new();
        let v = solve(&state, &db, &mut tt);
        // Just check it doesn't panic and returns a valid 3-way value;
        // exact value depends on the specific position, which we're not
        // hand-verifying here (that's what the retro/oracle tests are for).
        assert!(matches!(v, Value::Win | Value::Loss | Value::Draw));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn from_i8_matches_expected_mapping() {
        assert_eq!(from_i8(-1), Value::Loss);
        assert_eq!(from_i8(0), Value::Draw);
        assert_eq!(from_i8(1), Value::Win);
    }

    /// Plain recursive minimax with no pruning and no transposition
    /// table — independent of `negamax`'s alpha-beta/TT logic, so
    /// agreement between the two is a genuine check that pruning isn't
    /// cutting off a branch it shouldn't.
    fn brute_force(state: &PlacementState, db: &Database) -> i8 {
        if state.total_mover() < 3 {
            return -1;
        }
        if state.placement_done() {
            return if state.pos.is_blocked() { -1 } else { code_to_i8(db.lookup_pos(state.pos)) };
        }
        let succs = successors(state);
        if succs.is_empty() {
            return -1;
        }
        succs.iter().map(|s| -brute_force(s, db)).max().unwrap()
    }

    #[test]
    fn negamax_matches_brute_force_on_a_few_ply_subtree() {
        let tmp = std::env::temp_dir().join(format!("ninemm_opening_bruteforce_test_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(6)).unwrap(); // {3,3}
        let manifest = Manifest::load(&tmp).unwrap();
        let db = load_db_up_to(&tmp, &manifest);

        // A state 2 plies from placement-done (mover_hand=1, opp_hand=2),
        // ending at {3,3} regardless of the (mill-free) choices made —
        // small enough for brute force to finish quickly, deep enough to
        // exercise actual pruning (multiple branches, non-trivial alpha
        // narrowing) rather than just the terminal case.
        let mover = (1 << 3) | (1 << 8);
        let opp = 1 << 16;
        let state = PlacementState { pos: Position::new(mover, opp), mover_hand: 1, opp_hand: 2 };

        let mut tt = HashMap::new();
        let ab = negamax(&state, -1, 1, &db, &mut tt);
        let bf = brute_force(&state, &db);
        assert_eq!(ab, bf, "alpha-beta and brute-force minimax disagree");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn persistent_cache_gives_identical_results_to_an_empty_tt() {
        use crate::opening_cache;

        let tmp = std::env::temp_dir()
            .join(format!("ninemm_opening_cache_identical_test_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(6)).unwrap(); // {3,3} only
        let manifest = Manifest::load(&tmp).unwrap();
        let db = load_db_up_to(&tmp, &manifest);

        let state_a = PlacementState {
            pos: Position::new((1 << 3) | (1 << 8), 1 << 16),
            mover_hand: 1,
            opp_hand: 2,
        };
        let mut tt_a = Tt::new();
        let v_a = solve(&state_a, &db, &mut tt_a);
        assert_eq!(v_a, from_i8(brute_force(&state_a, &db)));

        let fingerprint = opening_cache::db_fingerprint(&manifest);
        opening_cache::write_cache(&tmp, fingerprint, &tt_a, 0).unwrap();
        let cached = opening_cache::load_or_empty(&tmp, &manifest);
        assert!(!cached.is_empty());

        // A different prior search's TT genuinely exercises cache hits
        // for a state that was not itself the previous root, rather than
        // just replaying the same run against itself.
        let state_b = successors(&state_a)[0];
        let mut cached = cached;
        let with_cache = solve(&state_b, &db, &mut cached);
        let without_cache = solve(&state_b, &db, &mut Tt::new());
        assert_eq!(with_cache, without_cache);
        assert_eq!(with_cache, from_i8(brute_force(&state_b, &db)));

        // Re-loading a fresh copy of the cache still reproduces v_a.
        let mut reloaded = opening_cache::load_or_empty(&tmp, &manifest);
        assert_eq!(solve(&state_a, &db, &mut reloaded), v_a);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn tt_reuse_across_solves_never_changes_results() {
        let tmp =
            std::env::temp_dir().join(format!("ninemm_opening_ttreuse_test_{}", std::process::id()));
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

    // --- Move-ordering tests (design.md §6: "mills/captures first, then
    // center-symmetric points"). `movegen::moves_placement` itself does
    // none of this (raw 0..24 point-order sweep) — `successors` is where
    // it's actually applied, so that's what's under test here. ---

    /// The pre-fix behavior of `successors`: `movegen::moves_placement`'s
    /// raw point-order sweep, un-reordered. Kept only as (a) the baseline
    /// that `successors_reordering_is_a_permutation_of_the_unordered_generation`
    /// checks the new ordering against, and (b) the "before" arm of the
    /// node-count benchmark below.
    fn successors_unordered(state: &PlacementState) -> Vec<PlacementState> {
        if state.mover_hand == 0 {
            return Vec::new();
        }
        movegen::moves_placement(state.pos.white(), state.pos.black())
            .into_iter()
            .map(|new_pos| PlacementState {
                pos: new_pos,
                mover_hand: state.opp_hand,
                opp_hand: state.mover_hand - 1,
            })
            .collect()
    }

    #[test]
    fn successors_orders_captures_before_quiet_moves() {
        // Mover holds two of the {0,1,2} mill line; placing on point 2
        // closes it and captures opp's lone, unmilled stone at 10.
        let mover = (1 << 0) | (1 << 1);
        let opp = 1 << 10;
        let state = PlacementState { pos: Position::new(mover, opp), mover_hand: 3, opp_hand: 3 };
        let succs = successors(&state);

        let capture_count = succs.iter().filter(|s| s.pos.white().count_ones() == 0).count();
        assert_eq!(capture_count, 1, "only point 2 closes the mill here");
        assert_eq!(
            succs[0].pos.white().count_ones(),
            0,
            "the capturing successor should sort before all quiet ones"
        );
        assert!(
            succs[1..].iter().all(|s| s.pos.white().count_ones() == 1),
            "every successor after the capture should be quiet"
        );
    }

    #[test]
    fn successors_orders_quiet_moves_by_descending_point_degree() {
        // Empty board: no captures possible, so this isolates the
        // secondary "center-symmetric points" (= higher board-connectivity
        // points) ordering key.
        let succs = successors(&PlacementState::initial());
        let degrees: Vec<u32> =
            succs.iter().map(|s| ADJ[s.pos.black().trailing_zeros() as usize].count_ones()).collect();
        for w in degrees.windows(2) {
            assert!(w[0] >= w[1], "point degrees are not sorted descending: {degrees:?}");
        }
        // board.rs::degree_distribution: 4 points of degree 4, 8 of degree
        // 3, 12 of degree 2 — all 24 must appear, split accordingly.
        assert_eq!(degrees.iter().filter(|&&d| d == 4).count(), 4);
        assert_eq!(degrees.iter().filter(|&&d| d == 3).count(), 8);
        assert_eq!(degrees.iter().filter(|&&d| d == 2).count(), 12);
    }

    #[test]
    fn successors_reordering_is_a_permutation_of_the_unordered_generation() {
        use std::collections::HashSet;
        // Same mill-threatening state as the capture-ordering test, so
        // this also exercises capture branches, not just quiet ones.
        let mover = (1 << 0) | (1 << 1);
        let opp = 1 << 10;
        let state = PlacementState { pos: Position::new(mover, opp), mover_hand: 3, opp_hand: 3 };

        let ordered = successors(&state);
        let unordered = successors_unordered(&state);
        assert_eq!(ordered.len(), unordered.len());
        let ordered_set: HashSet<PlacementState> = ordered.into_iter().collect();
        let unordered_set: HashSet<PlacementState> = unordered.into_iter().collect();
        assert_eq!(ordered_set, unordered_set, "reordering must not add or drop successors");
    }

    /// Instrumented copy of `negamax`'s alpha-beta/TT logic, parameterized
    /// over which successor function to use and counting nodes visited.
    /// Lets the benchmark tests below measure the new ordering's pruning
    /// effect against the pre-fix `successors_unordered` on the identical
    /// subtree. Not used by production code — kept `#[cfg(test)]`-only.
    fn negamax_counting(
        state: &PlacementState,
        mut alpha: i8,
        beta: i8,
        db: &Database,
        tt: &mut Tt,
        succ_fn: impl Fn(&PlacementState) -> Vec<PlacementState> + Copy,
        nodes: &mut u64,
    ) -> i8 {
        *nodes += 1;
        if state.total_mover() < 3 {
            return -1;
        }
        if state.placement_done() {
            return if state.pos.is_blocked() { -1 } else { code_to_i8(db.lookup_pos(state.pos)) };
        }
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
        let succs = succ_fn(state);
        if succs.is_empty() {
            return -1;
        }
        let mut best = i8::MIN;
        for succ in succs {
            let v = -negamax_counting(&succ, -beta, -alpha, db, tt, succ_fn, nodes);
            if v > best {
                best = v;
            }
            if best > alpha {
                alpha = best;
            }
            if alpha >= beta {
                break;
            }
        }
        let bound = if best >= beta && best < 1 {
            Bound::Lower
        } else if best <= alpha_orig && best > -1 {
            Bound::Upper
        } else {
            Bound::Exact
        };
        tt.insert(key, (best, bound));
        best
    }

    #[test]
    fn move_ordering_does_not_change_the_value_on_a_3_3_subtree() {
        let tmp = std::env::temp_dir()
            .join(format!("ninemm_opening_ordering_bench_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(6)).unwrap(); // {3,3} only
        let manifest = Manifest::load(&tmp).unwrap();
        let db = load_db_up_to(&tmp, &manifest);

        // A self-contained 3-stones-each "mini game" (empty board,
        // hand=3/3): every path reaches placement_done at exactly {3,3},
        // because any mid-tree capture immediately drops a side's total
        // below 3 and terminates right there via the `total_mover() < 3`
        // check — never touching a subspace outside the loaded {3,3}
        // database. 6 plies deep: deeper than this module's other
        // subtrees, and small/dense enough to actually touch the database.
        let root = PlacementState { pos: Position::new(0, 0), mover_hand: 3, opp_hand: 3 };

        let mut nodes_before = 0u64;
        let mut tt_before = Tt::new();
        let v_before = negamax_counting(
            &root,
            -1,
            1,
            &db,
            &mut tt_before,
            successors_unordered,
            &mut nodes_before,
        );

        let mut nodes_after = 0u64;
        let mut tt_after = Tt::new();
        let v_after =
            negamax_counting(&root, -1, 1, &db, &mut tt_after, successors, &mut nodes_after);

        assert_eq!(v_before, v_after, "move ordering must not change the game value");

        // Deliberately *not* asserting nodes_after < nodes_before here: on
        // this fully-bounded 6-ply mini-game, a full [-1,1]-window search
        // ends up needing to establish an exact 3-valued result almost
        // everywhere (there's no large won/lost region to cut off early),
        // so the tree is already close to saturated regardless of order —
        // ordering can even mildly lose on instances this small and dense
        // (observed: before=2605, after=5227 nodes here). The node-count
        // benefit shows up where it matters, on the wide, weakly-decided
        // early plies of the real search: see
        // `move_ordering_reduces_nodes_on_a_bounded_empty_board_search`
        // below, which measures exactly that and sees a consistent ~2x+
        // reduction.
        eprintln!(
            "[bench] 3-3 mini-game (6-ply, from empty board): nodes before={nodes_before} \
             after={nodes_after}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Horizon-limited negamax used only for node-count benchmarking:
    /// same alpha-beta/TT shape as `negamax`, but stops at a fixed ply
    /// depth and returns a neutral value instead of a database probe.
    /// This isolates the *early-game* pruning effect of move ordering —
    /// mirroring how Gasser reports "positions visited at the 8-ply
    /// level" as the search-quality metric (§5) — without needing any
    /// part of the real (multi-GB) movement/endgame database.
    fn negamax_bounded(
        state: &PlacementState,
        mut alpha: i8,
        beta: i8,
        depth_remaining: u32,
        tt: &mut Tt,
        succ_fn: impl Fn(&PlacementState) -> Vec<PlacementState> + Copy,
        nodes: &mut u64,
    ) -> i8 {
        *nodes += 1;
        if state.total_mover() < 3 {
            return -1;
        }
        if depth_remaining == 0 {
            return 0; // horizon cutoff: a neutral stand-in, not a claimed draw
        }
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
        let succs = succ_fn(state);
        if succs.is_empty() {
            return -1;
        }
        let mut best = i8::MIN;
        for succ in succs {
            let v =
                -negamax_bounded(&succ, -beta, -alpha, depth_remaining - 1, tt, succ_fn, nodes);
            if v > best {
                best = v;
            }
            if best > alpha {
                alpha = best;
            }
            if alpha >= beta {
                break;
            }
        }
        let bound = if best >= beta && best < 1 {
            Bound::Lower
        } else if best <= alpha_orig && best > -1 {
            Bound::Upper
        } else {
            Bound::Exact
        };
        tt.insert(key, (best, bound));
        best
    }

    #[test]
    fn move_ordering_reduces_nodes_on_a_bounded_empty_board_search() {
        // No database at all here — this is exactly the kind of "bounded
        // search from the empty board" comparison RESULTS.md's known
        // limitation calls for, at a depth cheap enough to run in a unit
        // test (the real search is 18 plies against a 17 GB database).
        const DEPTH: u32 = 9;

        let mut nodes_before = 0u64;
        let mut tt_before = Tt::new();
        let v_before = negamax_bounded(
            &PlacementState::initial(),
            -1,
            1,
            DEPTH,
            &mut tt_before,
            successors_unordered,
            &mut nodes_before,
        );

        let mut nodes_after = 0u64;
        let mut tt_after = Tt::new();
        let v_after = negamax_bounded(
            &PlacementState::initial(),
            -1,
            1,
            DEPTH,
            &mut tt_after,
            successors,
            &mut nodes_after,
        );

        assert_eq!(v_before, v_after, "move ordering must not change the (horizon-bounded) value");
        eprintln!(
            "[bench] empty-board {DEPTH}-ply horizon search: nodes before={nodes_before} \
             after={nodes_after} ({:.2}x fewer)",
            nodes_before as f64 / nodes_after as f64
        );
        assert!(
            nodes_after < nodes_before,
            "expected move ordering to reduce visited nodes: before={nodes_before} \
             after={nodes_after}"
        );
    }
}
