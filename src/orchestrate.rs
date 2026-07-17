//! Walks the 28-pair solve DAG (design.md §5), loading each pair's (at
//! most two) direct dependencies from disk, solving, and persisting
//! results — resumable via the manifest, so a partial run can be
//! continued later without re-solving finished pairs.

use crate::index::solve_order;
use crate::persist::{self, Manifest};
use crate::retro::{self, Database};
use anyhow::Result;
use std::path::Path;
use std::time::Instant;

/// Solve every pair in the DAG (or, with `max_total`, every pair whose
/// combined stone count is at or below that limit — useful for partial
/// runs and benchmarking). Skips pairs already solved on disk with a
/// checksum matching the manifest.
pub fn solve_all(dir: &Path, max_total: Option<usize>) -> Result<()> {
    let mut manifest = Manifest::load(dir)?;

    for (a, b) in solve_order() {
        if let Some(mt) = max_total {
            if a + b > mt {
                break;
            }
        }

        let already_done = if a == b {
            persist::is_solved(dir, &manifest, a, b)
        } else {
            persist::is_solved(dir, &manifest, a, b) && persist::is_solved(dir, &manifest, b, a)
        };
        if already_done {
            eprintln!("[{a}-{b}] already solved, skipping");
            continue;
        }

        let t_load = Instant::now();
        let mut db = Database::new();
        // Dependencies per Gasser's Figure 4 DAG: capturing from subspace
        // (a,b) lands in (b-1,a) — part of pair {a,b-1}; capturing from
        // subspace (b,a) lands in (a-1,b) — part of pair {a-1,b}. Neither
        // exists when the relevant side would drop below 3 stones.
        if b >= 4 {
            let data = persist::read_subspace_verified(dir, &manifest, b - 1, a)?;
            db.insert(b - 1, a, data);
        }
        if a >= 4 {
            let data = persist::read_subspace_verified(dir, &manifest, a - 1, b)?;
            db.insert(a - 1, b, data);
        }
        eprintln!("[{a}-{b}] loaded dependencies in {:?}", t_load.elapsed());

        let t_solve = Instant::now();
        let result = retro::solve_pair(a, b, &db);
        let solve_dt = t_solve.elapsed();

        let (wins, losses, draws) = tally(&result.val_ab);
        eprintln!(
            "[{a}-{b}] solved {} states in {:?} (wins={wins} losses={losses} draws={draws})",
            result.val_ab.len(),
            solve_dt
        );

        let t_write = Instant::now();
        let e1 = persist::write_subspace(dir, a, b, &result.val_ab)?;
        manifest.upsert(e1);
        if a != b {
            let e2 = persist::write_subspace(dir, b, a, &result.val_ba)?;
            manifest.upsert(e2);
        }
        manifest.save(dir)?;
        eprintln!("[{a}-{b}] wrote + checksummed in {:?}", t_write.elapsed());
    }

    Ok(())
}

fn tally(values: &[u16]) -> (u64, u64, u64) {
    let (mut wins, mut losses, mut draws) = (0u64, 0u64, 0u64);
    for &v in values {
        if v == retro::DRAW {
            draws += 1;
        } else if v % 2 == 0 {
            losses += 1;
        } else {
            wins += 1;
        }
    }
    (wins, losses, draws)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solve_all_up_to_3_3_produces_expected_file() {
        let tmp = std::env::temp_dir().join(format!("ninemm_orch_test_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        solve_all(&tmp, Some(6)).unwrap(); // only the {3,3} pair has total 6
        let manifest = Manifest::load(&tmp).unwrap();
        assert!(persist::is_solved(&tmp, &manifest, 3, 3));
        let data = persist::read_subspace_verified(&tmp, &manifest, 3, 3).unwrap();
        assert_eq!(data.len(), crate::index::subspace_size(crate::index::SubspaceId::new(3, 3)) as usize);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn solve_all_is_resumable() {
        let tmp = std::env::temp_dir().join(format!("ninemm_orch_test2_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        solve_all(&tmp, Some(6)).unwrap();
        // Corrupt nothing; just re-run and confirm it skips (fast) and
        // still ends with a valid, checksummed file.
        solve_all(&tmp, Some(6)).unwrap();
        let manifest = Manifest::load(&tmp).unwrap();
        assert!(persist::is_solved(&tmp, &manifest, 3, 3));
        std::fs::remove_dir_all(&tmp).ok();
    }
}
