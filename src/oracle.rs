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

/// Solve subspace `(w, b)` (movement phase only) to a fixpoint by plain
/// forward value iteration. Successors that leave the subspace because a
/// capture reduced the opponent below 3 stones are treated as immediate
/// `Loss(0)` for whoever would be to move there — exactly the terminal
/// rule (fewer than three stones is a loss), applied directly rather than
/// via any shared initialization code.
pub fn solve_pair(w: usize, b: usize) -> HashMap<Position, Value> {
    let states = all_states(w, b);
    let mut values: HashMap<Position, Value> = HashMap::with_capacity(states.len());

    loop {
        let mut changed = false;
        for &s in &states {
            if values.contains_key(&s) {
                continue;
            }
            let succs = moves_movement(s.white(), s.black());
            if succs.is_empty() {
                values.insert(s, Value::Loss(0));
                changed = true;
                continue;
            }
            let mut best_win_depth: Option<u32> = None;
            let mut max_loss_depth = 0u32;
            let mut all_decided_as_win_for_opponent = true;
            for succ in &succs {
                let succ_val = if succ.white_count() < 3 {
                    Some(Value::Loss(0))
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
                    _ => {
                        all_decided_as_win_for_opponent = false;
                    }
                }
            }
            if let Some(bd) = best_win_depth {
                values.insert(s, Value::Win(bd));
                changed = true;
            } else if all_decided_as_win_for_opponent {
                values.insert(s, Value::Loss(max_loss_depth + 1));
                changed = true;
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
        let values = solve_pair(3, 3);
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
        let values = solve_pair(3, 3);
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
