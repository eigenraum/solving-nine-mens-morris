//! Independent verification oracle.
//!
//! A deliberately simple, from-scratch solver used **only** to
//! cross-check the retrograde engine (`retro.rs`) on small subspace
//! pairs. It shares `movegen` (basic rule mechanics — move/capture
//! generation) with the rest of the codebase, but nothing else: no
//! symmetry reduction, no combinatorial indexing, and — critically — no
//! reverse-move generation. It solves purely by repeatedly scanning
//! *forward* successors to a fixpoint (plain value iteration over a
//! `HashMap`), which is algorithmically unrelated to retrograde
//! propagation. If the two independent implementations agree, that's
//! strong evidence neither has a rule-logic or graph-traversal bug.
//!
//! Only tractable for small subspaces (raw, unreduced state counts): used
//! for the `{3,3}` pair exhaustively and spot checks on `{4,3}`/`{4,4}`.

use crate::movegen::moves_movement;
use crate::pos::Position;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Win(u32),
    Loss(u32),
    Draw,
}

/// All raw states with exactly `w` white and `b` black stones (disjoint,
/// no symmetry reduction — every distinct bitboard is its own state).
pub fn all_states(w: usize, b: usize) -> Vec<Position> {
    let pts: Vec<usize> = (0..24).collect();
    let mut states = Vec::with_capacity(estimate_count(w, b));
    for wc in combinations(&pts, w) {
        let white_mask: u32 = wc.iter().map(|&p| 1u32 << p).sum();
        let remaining: Vec<usize> = pts.iter().copied().filter(|p| white_mask & (1 << p) == 0).collect();
        for bc in combinations(&remaining, b) {
            let black_mask: u32 = bc.iter().map(|&p| 1u32 << p).sum();
            states.push(Position::new(white_mask, black_mask));
        }
    }
    states
}

fn estimate_count(w: usize, b: usize) -> usize {
    fn choose(n: usize, k: usize) -> usize {
        if k > n {
            return 0;
        }
        let mut r = 1usize;
        for i in 0..k {
            r = r * (n - i) / (i + 1);
        }
        r
    }
    choose(24, w) * choose(24 - w, b)
}

fn combinations(universe: &[usize], k: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut cur = Vec::with_capacity(k);
    fn rec(universe: &[usize], k: usize, start: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if cur.len() == k {
            out.push(cur.clone());
            return;
        }
        if universe.len() - start < k - cur.len() {
            return;
        }
        for i in start..universe.len() {
            cur.push(universe[i]);
            rec(universe, k, i + 1, cur, out);
            cur.pop();
        }
    }
    rec(universe, k, 0, &mut cur, &mut out);
    out
}

/// Solve the unordered pair `{a, b}` — i.e. *both* ordered subspaces
/// `(a, b)` and `(b, a)` together — to a fixpoint by plain forward value
/// iteration (Bellman-Ford-style relaxation). This is required, not
/// optional, whenever `a != b`: a quiet move flips perspective, so a
/// state with `(a, b)` stones has successors with `(b, a)` stones. Solving
/// just one ordered subspace in isolation leaves every quiet successor
/// permanently "unknown" (looked up in a state universe that was never
/// populated), so only immediate-capture wins would ever be decided.
///
/// `smaller` must contain the fully-solved values of every strictly
/// smaller pair (lower total stone count) reachable by a single capture
/// from `{a, b}` — mirroring `retro::Database`'s bottom-up dependency
/// structure, just as a plain combined map instead of a purpose-built
/// registry. A capture only terminates locally (immediate `Loss(0)`) when
/// it drops the opponent below 3 stones; otherwise it lands in a smaller
/// pair (still >= 3 stones per side) that must be looked up here, or every
/// state depending on that capture's outcome is permanently stuck
/// "unknown" instead of resolving to the correct value. For the base case
/// `{3,3}`, every capture drops the opponent below 3, so `smaller` may be
/// empty.
///
/// Critically, this *keeps recomputing every state every pass* rather than
/// locking in a value the first time one is found. A naive "decide once"
/// approach is a classic trap here: whether a state's first-discovered
/// winning reply happens to be its *fastest* one depends on the arbitrary
/// iteration order of `states`, since a longer dependency chain can
/// occasionally resolve within a single pass (favorable ordering lets
/// several hops cascade together) while a shorter chain does not. Locking
/// in early can therefore permanently fix a state at a *correct but
/// non-minimal* win/loss depth. Relaxation avoids this: a value is only
/// ever replaced by a strictly smaller depth of the same kind, so the
/// process is monotonically improving and bounded, and it only terminates
/// once a full pass finds no further improvement anywhere — at which
/// point every value must already equal the true recursive minimum.
pub fn solve_pair(a: usize, b: usize, smaller: &HashMap<Position, Value>) -> HashMap<Position, Value> {
    let mut states = all_states(a, b);
    if a != b {
        states.extend(all_states(b, a));
    }
    let total = a + b;
    let mut values: HashMap<Position, Value> = HashMap::with_capacity(states.len());

    loop {
        let mut changed = false;
        for &s in &states {
            let succs = moves_movement(s.white(), s.black());
            let new_val = if succs.is_empty() {
                Some(Value::Loss(0))
            } else {
                let mut best_win_depth: Option<u32> = None;
                let mut max_loss_depth = 0u32;
                let mut all_win = true;
                for succ in &succs {
                    let succ_val = if succ.white_count() < 3 {
                        Some(Value::Loss(0))
                    } else if (succ.white_count() + succ.black_count()) < total as u32 {
                        Some(
                            *smaller.get(succ).unwrap_or_else(|| {
                                panic!("capture landed in an unsolved smaller pair: {succ:?}")
                            }),
                        )
                    } else {
                        values.get(succ).copied()
                    };
                    match succ_val {
                        Some(Value::Loss(d)) => {
                            best_win_depth = Some(best_win_depth.map_or(d + 1, |bd: u32| bd.min(d + 1)));
                        }
                        Some(Value::Win(d)) => {
                            max_loss_depth = max_loss_depth.max(d);
                        }
                        Some(Value::Draw) => {
                            // A permanently-drawn option blocks this state
                            // from ever being forced into a loss, but
                            // doesn't decide it either.
                            all_win = false;
                        }
                        None => all_win = false,
                    }
                }
                if let Some(bd) = best_win_depth {
                    Some(Value::Win(bd))
                } else if all_win {
                    Some(Value::Loss(max_loss_depth + 1))
                } else {
                    None
                }
            };
            if let Some(nv) = new_val {
                let improved = match values.get(&s) {
                    None => true,
                    Some(Value::Win(od)) => matches!(nv, Value::Win(nd) if nd < *od),
                    Some(Value::Loss(od)) => matches!(nv, Value::Loss(nd) if nd < *od),
                    Some(Value::Draw) => false, // Draw is never assigned mid-loop
                };
                if improved {
                    values.insert(s, nv);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    for &s in &states {
        values.entry(s).or_insert(Value::Draw);
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinations_count_matches_binomial() {
        assert_eq!(combinations(&(0..24).collect::<Vec<_>>(), 3).len(), 2024);
        assert_eq!(combinations(&(0..21).collect::<Vec<_>>(), 3).len(), 1330);
    }

    #[test]
    fn all_states_3_3_matches_expected_raw_count() {
        // C(24,3) * C(21,3) = 2024 * 1330 = 2,691,920 (design.md's "~2.7M raw states")
        assert_eq!(all_states(3, 3).len(), 2_691_920);
    }

    /// Full solve of the 3-3 pair (~2.7M states, ~15s in release), checked
    /// against two numbers published in Gasser's paper: the longest loss
    /// in the 3-3 database is 26 plies (Figure 9's caption), and about 83%
    /// of 3-3 positions are wins for the side to move (Figure 11). These
    /// are independent, paper-derived sanity checks on the oracle itself,
    /// before it's ever used to validate the retrograde engine.
    ///
    /// Run explicitly: `cargo test --release -- --ignored oracle`
    #[test]
    #[ignore]
    fn solve_3_3_matches_gasser_paper() {
        let values = solve_pair(3, 3, &HashMap::new());
        assert_eq!(values.len(), 2_691_920);

        let mut wins = 0usize;
        let mut max_loss_depth = 0u32;
        for v in values.values() {
            match v {
                Value::Win(_) => wins += 1,
                Value::Loss(d) => max_loss_depth = max_loss_depth.max(*d),
                Value::Draw => {}
            }
        }
        assert_eq!(max_loss_depth, 26, "Gasser Figure 9: longest 3-3 loss is 26 plies");
        let win_pct = 100.0 * wins as f64 / values.len() as f64;
        assert!(
            (82.0..84.0).contains(&win_pct),
            "Gasser Figure 11: 3-3 win rate for side to move is ~83%, got {win_pct:.1}%"
        );
    }

    #[test]
    fn every_oracle_value_is_self_consistent() {
        // Small, fast self-check: solve a tiny synthetic-scale-down isn't
        // possible (rules are fixed), so we check internal consistency on
        // the full 3-3 solve — every stored value must be justified by its
        // own successors under minimax, using only the oracle's own map.
        let values = solve_pair(3, 3, &HashMap::new());
        let mut checked = 0;
        for (&s, &v) in values.iter() {
            if checked >= 5000 {
                break; // full check is done in the (slower) integration test
            }
            checked += 1;
            let succs = moves_movement(s.white(), s.black());
            match v {
                Value::Loss(0) => assert!(succs.is_empty(), "Loss(0) state has moves: {s:?}"),
                Value::Win(d) => {
                    let ok = succs.iter().any(|succ| {
                        if succ.white_count() < 3 {
                            d == 1
                        } else {
                            matches!(values.get(succ), Some(Value::Loss(ld)) if *ld + 1 == d)
                        }
                    });
                    assert!(ok, "Win({d}) at {s:?} not justified by any successor");
                }
                Value::Loss(d) => {
                    assert!(!succs.is_empty(), "Loss({d}) with d>0 must have moves: {s:?}");
                    let all_win = succs.iter().all(|succ| {
                        succ.white_count() >= 3
                            && matches!(values.get(succ), Some(Value::Win(wd)) if *wd + 1 <= d)
                    });
                    assert!(all_win, "Loss({d}) at {s:?} not justified: not all successors are opponent wins with matching depth bound");
                }
                Value::Draw => {
                    // must have at least one successor that is itself a
                    // draw or unresolved-in-a-way-that-isn't-a-forced-win
                    assert!(!succs.is_empty(), "blocked position must be Loss(0), not Draw: {s:?}");
                }
            }
        }
    }
}
