//! Forward-consistency verification (design.md §8.1).
//!
//! Deliberately independent of the retrograde propagation machinery in
//! `retro.rs`: for every stored state, recompute the minimax value
//! directly from its successors' *already-stored* values (a single pass,
//! no bucket queue, no Count/max_seen_depth bookkeeping) and check it
//! matches what's on disk. This exercises a completely different code
//! path than the solver that produced the data, so it can catch classes
//! of bugs the solver's own logic would be blind to.

use crate::index::{self, SubspaceId};
use crate::movegen::moves_movement;
use crate::persist::{self, Manifest};
use crate::pos::Position;
use crate::retro::{self, Database, DRAW};
use anyhow::Result;
use rayon::prelude::*;
use std::path::Path;

pub struct Mismatch {
    pub idx: u64,
    pub stored: u16,
    pub expected: u16,
}

pub struct PairVerifyReport {
    pub a: usize,
    pub b: usize,
    pub checked: u64,
    pub mismatches: Vec<Mismatch>,
}

impl PairVerifyReport {
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Recompute the expected value of a state from its successors' stored
/// values: `other` is the sibling subspace within the same pair (where
/// quiet successors land), `db` holds already-verified-solved smaller
/// pairs (where capturing successors land).
fn expected_value(successors: &[Position], total: usize, other: &[u16], db: &Database) -> u16 {
    if successors.is_empty() {
        return 0; // Loss(0)
    }
    let mut best_win: Option<u16> = None;
    let mut all_win = true;
    let mut max_loss_seen = 0u16;

    for succ in successors {
        let code = if succ.white_count() < 3 {
            0 // opponent reduced below 3: Loss(0) for succ's mover
        } else {
            let succ_total = (succ.white_count() + succ.black_count()) as usize;
            if succ_total < total {
                db.lookup_pos(*succ)
            } else {
                let (_sub, idx) = index::index(*succ);
                other[idx as usize]
            }
        };
        if code == DRAW {
            all_win = false;
        } else if code % 2 == 0 {
            let d = code + 1;
            best_win = Some(best_win.map_or(d, |bd: u16| bd.min(d)));
        } else {
            max_loss_seen = max_loss_seen.max(code);
        }
    }

    if let Some(d) = best_win {
        d
    } else if all_win {
        max_loss_seen + 1
    } else {
        DRAW
    }
}

fn verify_side(sub: SubspaceId, own: &[u16], other: &[u16], db: &Database) -> (u64, Vec<Mismatch>) {
    let total = sub.w as usize + sub.b as usize;
    let results: Vec<(u64, Option<Mismatch>)> = (0..own.len() as u64)
        .into_par_iter()
        .filter_map(|idx| {
            if !index::is_canonical_slot(sub, idx) {
                return None;
            }
            let pos = index::unindex(sub, idx);
            let successors = moves_movement(pos.white(), pos.black());
            let expected = expected_value(&successors, total, other, db);
            let stored = own[idx as usize];
            if stored != expected {
                Some((idx, Some(Mismatch { idx, stored, expected })))
            } else {
                Some((idx, None))
            }
        })
        .collect();

    let checked = results.len() as u64;
    let mismatches = results.into_iter().filter_map(|(_, m)| m).collect();
    (checked, mismatches)
}

/// Verify one unordered pair `{a, b}`. Requires the pair's own file(s)
/// and its (at most two) dependency files already on disk and
/// checksummed in the manifest.
pub fn verify_pair(dir: &Path, manifest: &Manifest, a: usize, b: usize) -> Result<PairVerifyReport> {
    let val_ab = persist::read_subspace_verified(dir, manifest, a, b)?;
    let val_ba = if a == b {
        val_ab.clone()
    } else {
        persist::read_subspace_verified(dir, manifest, b, a)?
    };

    let mut db = Database::new();
    if b >= 4 {
        db.insert(b - 1, a, persist::read_subspace_verified(dir, manifest, b - 1, a)?);
    }
    if a >= 4 {
        db.insert(a - 1, b, persist::read_subspace_verified(dir, manifest, a - 1, b)?);
    }

    let mut checked = 0u64;
    let mut mismatches = Vec::new();

    let (c1, m1) = verify_side(SubspaceId::new(a, b), &val_ab, &val_ba, &db);
    checked += c1;
    mismatches.extend(m1);

    if a != b {
        let (c2, m2) = verify_side(SubspaceId::new(b, a), &val_ba, &val_ab, &db);
        checked += c2;
        mismatches.extend(m2);
    }

    mismatches.truncate(50); // cap for reporting; `checked` still reflects the full scan
    Ok(PairVerifyReport { a, b, checked, mismatches })
}

/// Verify every pair solved so far, in DAG order (so each pair's
/// dependencies are already known-good by the time it's checked).
pub fn verify_all(dir: &Path) -> Result<Vec<PairVerifyReport>> {
    let manifest = Manifest::load(dir)?;
    let mut reports = Vec::new();
    for (a, b) in index::solve_order() {
        let done = if a == b {
            persist::is_solved(dir, &manifest, a, b)
        } else {
            persist::is_solved(dir, &manifest, a, b) && persist::is_solved(dir, &manifest, b, a)
        };
        if !done {
            break; // DAG order: nothing later can be solved without this
        }
        let report = verify_pair(dir, &manifest, a, b)?;
        let ok = report.ok();
        reports.push(report);
        if !ok {
            break; // stop at the first inconsistency rather than cascading
        }
    }
    Ok(reports)
}

/// Win/loss/draw tallies plus the deepest win and loss depth in a
/// subspace — used by the `stats` command and as a sanity check against
/// Gasser's published figures.
pub struct Tally {
    pub wins: u64,
    pub losses: u64,
    pub draws: u64,
    pub max_win_depth: u16,
    pub max_loss_depth: u16,
}

/// Tally only over *canonical* slots — the stored array also contains
/// "wasted" slots (design.md §3: index positions whose canonical white
/// set has a nontrivial symmetry stabilizer, which permanently stay
/// `DRAW` since nothing ever indexes into them). Including those would
/// inflate the draw count and skew win/loss percentages away from
/// Gasser's published per-subspace figures, which are computed over the
/// true (non-redundant) state space.
pub fn tally(sub: SubspaceId, values: &[u16]) -> Tally {
    let mut t = Tally { wins: 0, losses: 0, draws: 0, max_win_depth: 0, max_loss_depth: 0 };
    for (idx, &v) in values.iter().enumerate() {
        if !index::is_canonical_slot(sub, idx as u64) {
            continue;
        }
        if v == retro::DRAW {
            t.draws += 1;
        } else if v % 2 == 0 {
            t.losses += 1;
            t.max_loss_depth = t.max_loss_depth.max(v);
        } else {
            t.wins += 1;
            t.max_win_depth = t.max_win_depth.max(v);
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate;

    #[test]
    fn verify_matches_freshly_solved_3_3_and_4_3() {
        let tmp = std::env::temp_dir().join(format!("ninemm_verify_test_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(7)).unwrap(); // {3,3} and {3,4}/{4,3}

        let reports = verify_all(&tmp).unwrap();
        assert!(!reports.is_empty());
        for r in &reports {
            assert!(r.ok(), "pair {}-{} has mismatches: {:?}", r.a, r.b, r.mismatches.iter().map(|m| (m.idx, m.stored, m.expected)).collect::<Vec<_>>());
            assert!(r.checked > 0);
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn tally_matches_known_gasser_figures_for_3_3() {
        let tmp = std::env::temp_dir().join(format!("ninemm_verify_tally_test_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(6)).unwrap();
        let manifest = Manifest::load(&tmp).unwrap();
        let data = persist::read_subspace_verified(&tmp, &manifest, 3, 3).unwrap();
        let t = tally(SubspaceId::new(3, 3), &data);
        assert_eq!(t.max_loss_depth, 26, "Gasser Figure 9: longest 3-3 loss is 26 plies");
        let total_canonical = t.wins + t.losses + t.draws;
        let win_pct = 100.0 * t.wins as f64 / total_canonical as f64;
        assert!((82.0..84.0).contains(&win_pct), "Gasser Figure 11: ~83% wins, got {win_pct:.1}%");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
