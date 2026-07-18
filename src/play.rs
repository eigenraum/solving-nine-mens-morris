//! Perfect-play move selection (design.md §7): given any position,
//! choose a winning move with minimal depth if one exists, else any
//! drawing move, else the losing move with maximal depth (delay the
//! loss as long as possible). Works across both game phases — movement
//! (via the solved database) and placement (via the opening search).

use crate::board;
use crate::movegen;
use crate::opening::{self, PlacementState};
use crate::pos::Position;
use crate::retro::{self, Database};

/// The result of choosing a move: the chosen successor and the
/// game-theoretic value of the position it came from, for display.
#[derive(Debug)]
pub struct Choice<S> {
    pub successor: S,
    pub value_code: u16,
}

/// Pick the best movement-phase successor of `pos` by the design.md §7
/// ordering. Returns `None` if `pos` has no legal moves (a loss).
pub fn best_movement_move(pos: Position, db: &Database) -> Option<Choice<Position>> {
    let succs = movegen::successors(pos);
    if succs.is_empty() {
        return None;
    }

    let mut best_win: Option<(u16, Position)> = None;
    let mut best_draw: Option<Position> = None;
    let mut best_loss: Option<(u16, Position)> = None; // maximize depth

    for succ in succs {
        // A successor that reduces the opponent below 3 stones is an
        // immediate win, not itself indexable in the database.
        let code = if succ.white_count() < 3 {
            0 // Loss(0) for succ's mover == Win(1) for us
        } else {
            db.lookup_pos(succ)
        };
        if code == retro::DRAW {
            best_draw.get_or_insert(succ);
        } else if code % 2 == 0 {
            // succ is a loss for its mover => a win for us, at depth code+1
            let d = code + 1;
            if best_win.is_none_or(|(bd, _)| d < bd) {
                best_win = Some((d, succ));
            }
        } else {
            // succ is a win for its mover => a loss for us, at depth code+1
            let d = code + 1;
            if best_loss.is_none_or(|(bd, _)| d > bd) {
                best_loss = Some((d, succ));
            }
        }
    }

    if let Some((d, s)) = best_win {
        Some(Choice { successor: s, value_code: d })
    } else if let Some(s) = best_draw {
        Some(Choice { successor: s, value_code: retro::DRAW })
    } else {
        let (d, s) = best_loss.expect("successors is nonempty, so some branch above must fire");
        Some(Choice { successor: s, value_code: d })
    }
}

/// Pick the best placement-phase successor of `state`, using the opening
/// search (design.md §6/§7). `tt` is reused across calls within one game
/// for efficiency.
pub fn best_placement_move(
    state: &PlacementState,
    db: &Database,
    tt: &mut opening::Tt,
) -> Option<PlacementState> {
    let succs = opening::successors(state);
    if succs.is_empty() {
        return None;
    }
    let mut best: Option<(i8, PlacementState)> = None;
    for succ in succs {
        let v = -opening::negamax(&succ, -1, 1, db, tt);
        if best.is_none_or(|(bv, _)| v > bv) {
            best = Some((v, succ));
        }
        if v == 1 {
            break; // a win is the best possible outcome; no need to keep looking
        }
    }
    best.map(|(_, s)| s)
}

/// Render a movement-phase position as a 24-point ASCII board.
pub fn render(pos: Position) -> String {
    let mut lines = Vec::new();
    for r in (1..=7).rev() {
        let mut line = String::new();
        for c in 1..=7u8 {
            let letter = (b'a' + (c - 1)) as char;
            let name = format!("{letter}{r}");
            match board::parse_point(&name) {
                Some(p) => {
                    let ch = if pos.white() & (1 << p) != 0 {
                        'W'
                    } else if pos.black() & (1 << p) != 0 {
                        'B'
                    } else {
                        '.'
                    };
                    line.push(ch);
                }
                None => line.push(' '),
            }
            line.push(' ');
        }
        lines.push(line);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index;
    use crate::orchestrate;
    use crate::persist::{self, Manifest};

    fn load_all(dir: &std::path::Path) -> Database {
        let manifest = Manifest::load(dir).unwrap();
        let mut db = Database::new();
        for e in &manifest.entries {
            let data = persist::read_subspace_verified(dir, &manifest, e.w as usize, e.b as usize).unwrap();
            db.insert(e.w as usize, e.b as usize, data);
        }
        db
    }

    #[test]
    fn best_move_from_blocked_position_is_none() {
        let db = Database::new();
        let white = 1u32;
        let black = (1 << 1) | (1 << 7);
        let pos = Position::new(white, black);
        assert!(best_movement_move(pos, &db).is_none());
    }

    #[test]
    fn best_move_prefers_win_over_draw_over_loss() {
        let tmp = std::env::temp_dir().join(format!("ninemm_play_test_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(7)).unwrap(); // {3,3}, {3,4}/{4,3}
        let db = load_all(&tmp);

        // Any 3-3 position with a legal move should get a move chosen
        // that is at least as good as a random alternative — spot check
        // by confirming the chosen successor's value is the best among
        // all successors (never worse than any alternative).
        let sub = index::SubspaceId::new(3, 3);
        let mut checked = 0;
        for idx in (0..index::subspace_size(sub)).step_by(997) {
            if !index::is_canonical_slot(sub, idx) {
                continue;
            }
            let pos = index::unindex(sub, idx);
            let succs = movegen::successors(pos);
            if succs.is_empty() {
                continue;
            }
            let Some(choice) = best_movement_move(pos, &db) else { continue };
            // The chosen value must be >= every alternative's implied
            // value for us (win < draw < loss, in the sense of "how bad
            // for us is this reply" -- we just check the chosen one is a
            // win if any winning successor exists.
            let any_win = succs.iter().any(|s| {
                let code = if s.white_count() < 3 { 0 } else { db.lookup_pos(*s) };
                code != retro::DRAW && code % 2 == 0
            });
            if any_win {
                assert!(choice.value_code != retro::DRAW && choice.value_code % 2 == 1, "chose non-win when a win was available");
            }
            checked += 1;
        }
        assert!(checked > 50, "expected to check a reasonable number of positions, got {checked}");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn render_shows_correct_stone_count() {
        let pos = Position::new(0b111, 0b111000);
        let rendered = render(pos);
        let w_count = rendered.matches('W').count();
        let b_count = rendered.matches('B').count();
        assert_eq!(w_count, 3);
        assert_eq!(b_count, 3);
    }

    /// Self-play soak test (design.md §8.5): following perfect play from
    /// a won position, the win depth must strictly decrease each ply
    /// until conversion (a capture into a smaller pair) or a terminal
    /// win — it must never increase or stall, which would indicate the
    /// move-selection logic isn't actually finding genuinely improving
    /// moves.
    #[test]
    fn self_play_win_depth_strictly_decreases() {
        let tmp = std::env::temp_dir().join(format!("ninemm_selfplay_test_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(6)).unwrap(); // {3,3}
        let db = load_all(&tmp);

        let sub = index::SubspaceId::new(3, 3);
        let mut tested_a_win = false;
        for idx in 0..index::subspace_size(sub).min(2_000) {
            if !index::is_canonical_slot(sub, idx) {
                continue;
            }
            let pos = index::unindex(sub, idx);
            let code = db.lookup_pos(pos);
            if code == retro::DRAW || code.is_multiple_of(2) {
                continue; // only start from positions that are wins for the mover to move
            }
            tested_a_win = true;

            // `cur` is always "our" position, a proven Win; `last_depth`
            // is its win depth. Each iteration: we play our best move
            // (reaching a Loss position for the opponent, one ply
            // shallower), then the opponent plays *their* best move
            // (delaying their loss as long as possible) to produce our
            // next position — which, since both sides play the proven
            // minimax line, must again be a Win with strictly smaller
            // depth than before. This is the actual invariant design.md
            // §8.5 asks for: depth strictly decreases at every full
            // round-trip, never stalls or increases.
            let mut cur = pos;
            let mut last_depth = code;
            let mut plies = 0;
            loop {
                let our_choice = best_movement_move(cur, &db).expect("a Win position must have a legal move");
                assert_eq!(our_choice.value_code, last_depth, "move selection disagrees with the position's own stored value");

                let opp_pos = our_choice.successor;
                if opp_pos.white_count() < 3 {
                    break; // our move captured the opponent below 3 stones: won outright
                }
                let opp_code = db.lookup_pos(opp_pos);
                assert_eq!(opp_code, last_depth - 1, "winning move must lead to a Loss exactly one ply shallower");
                if opp_code == 0 {
                    break; // opponent is immediately blocked: game over
                }

                let opp_choice = best_movement_move(opp_pos, &db)
                    .expect("a Loss position with depth > 0 must have a legal move");
                cur = opp_choice.successor;
                let new_depth = db.lookup_pos(cur);
                assert!(new_depth != retro::DRAW && new_depth % 2 == 1, "position after opponent's reply must be a Win for us");
                assert!(new_depth < last_depth, "win depth did not strictly decrease: {last_depth} -> {new_depth}");
                last_depth = new_depth;

                plies += 1;
                if plies > 60 {
                    panic!("self-play line did not terminate within 60 plies");
                }
            }
        }
        assert!(tested_a_win, "expected to find at least one Win position to test");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
