//! Retrograde analysis engine (single-threaded; M6 parallelizes this).
//!
//! Solves one unordered material pair `{a, b}` at a time — i.e. both
//! ordered subspaces `N(a,b)` and `N(b,a)` together, since a quiet move
//! flips perspective between them (design.md §5). Requires all
//! *strictly smaller* pairs (by total stone count) to already be solved
//! and registered in a [`Database`], because capturing moves land there.
//!
//! When `a == b`, subspaces `N(a,b)` and `N(b,a)` are literally the same
//! index space (not just isomorphic) — a quiet move stays inside it. This
//! is handled as a distinct "self-paired" case; treating it as two
//! separate arrays would silently misroute every predecessor update to
//! only one of them (they'd both claim to match every slot).
//!
//! ## Value encoding
//!
//! Unlike Gasser's one-byte Val/Count union (a necessity on his
//! memory-constrained hardware), we use a `u16` per state for the decided
//! value plus separate `u32`/`u16` scratch arrays during solving — modern
//! RAM makes the byte-packing unnecessary, and it removes a real
//! fragility (his scheme only works because max depth + max branching
//! factor stays under 255). A decided code equals its depth; depth parity
//! alone distinguishes win from loss (`Loss(d)` has even `d`, `Win(d)` has
//! odd `d` — true by induction, since each ply flips the mover and adds
//! exactly 1 to depth). `DRAW = u16::MAX` marks an undecided/final-draw
//! slot.
//!
//! ## Processing order and per-state bookkeeping
//!
//! States are decided in strict non-decreasing depth order using a
//! bucket queue (`Vec<Vec<slot>>` indexed by depth) rather than a single
//! FIFO queue: initial seeds from the init pass can already have *any*
//! depth (a state whose only successors are captures into an
//! already-solved smaller pair can be decided immediately, at whatever
//! depth that pair's values dictate) — not just depth 0 or 1 — so a plain
//! FIFO would not process in depth order and could compute wrong depths.
//!
//! For a pending state, three scratch values matter until it's decided:
//! - `count`: number of *quiet* (in-pair) successors not yet known to be
//!   an opponent win, plus one "phantom" (never-decremented) slot per
//!   capturing successor that is a permanent draw. The phantom slots mean
//!   a state with any drawn capturing option can never reach `count == 0`
//!   — correctly, since the mover can always choose that drawing move.
//! - `max_seen_depth`: running maximum depth over every successor proven
//!   to be an opponent win so far (both capturing ones, known instantly
//!   at init from the smaller pair, and quiet ones, discovered over the
//!   course of propagation). When `count` reaches 0, `Loss(max_seen_depth
//!   + 1)` is correct only because bucket-order processing guarantees the
//!   *last* successor to be accounted for has the maximum depth among all
//!   of them.

use crate::index::{self, SubspaceId};
use crate::movegen::{moves_movement, quiet_predecessors};
use crate::pos::Position;
use std::collections::HashMap;

pub const DRAW: u16 = u16::MAX;

#[inline]
pub fn is_loss(code: u16) -> bool {
    code != DRAW && code % 2 == 0
}

#[inline]
pub fn is_win(code: u16) -> bool {
    code != DRAW && code % 2 == 1
}

/// A registry of fully-solved subspaces, used for cross-pair capture
/// lookups (always into strictly smaller pairs, per the solve DAG).
#[derive(Default)]
pub struct Database {
    arrays: HashMap<(u8, u8), Vec<u16>>,
}

impl Database {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, w: usize, b: usize, values: Vec<u16>) {
        self.arrays.insert((w as u8, b as u8), values);
    }

    pub fn has(&self, w: usize, b: usize) -> bool {
        self.arrays.contains_key(&(w as u8, b as u8))
    }

    pub fn get(&self, sub: SubspaceId, idx: u64) -> u16 {
        self.arrays[&(sub.w, sub.b)][idx as usize]
    }

    /// Look up a position's value; it must already live in a registered
    /// (solved) subspace.
    pub fn lookup_pos(&self, pos: Position) -> u16 {
        let (sub, idx) = index::index(pos);
        self.get(sub, idx)
    }
}

pub struct PairResult {
    pub a: usize,
    pub b: usize,
    /// Values for subspace `(a, b)`.
    pub val_ab: Vec<u16>,
    /// Values for subspace `(b, a)`.
    pub val_ba: Vec<u16>,
}

/// One ordered subspace's mutable solving state.
struct Side {
    w: usize,
    b: usize,
    size: usize,
    val: Vec<u16>,
    count: Vec<u32>,
    max_seen_depth: Vec<u16>,
}

impl Side {
    fn new(w: usize, b: usize) -> Self {
        let size = index::subspace_size(SubspaceId::new(w, b)) as usize;
        Side {
            w,
            b,
            size,
            val: vec![DRAW; size],
            count: vec![0; size],
            max_seen_depth: vec![0; size],
        }
    }
}

fn push_bucket(buckets: &mut Vec<Vec<u64>>, depth: u16, idx: u64) {
    let d = depth as usize;
    if buckets.len() <= d {
        buckets.resize_with(d + 1, Vec::new);
    }
    buckets[d].push(idx);
}

/// Initialize one ordered subspace: mark terminal losses (no moves),
/// terminal wins (a capture reduces the opponent below 3 stones), and
/// resolve every capturing successor against the (already-solved)
/// smaller pair it lands in, immediately deciding any state whose
/// successors are already fully accounted for. Everything else gets a
/// `count`/`max_seen_depth` scratch entry and stays a pending draw
/// candidate until propagation resolves it (or leaves it a true draw).
/// Returns the list of (depth, idx) pairs decided during this pass.
fn init_side(side: &mut Side, db: &Database) -> Vec<(u16, u64)> {
    let sub = SubspaceId::new(side.w, side.b);
    let total = side.w + side.b;
    let mut decided = Vec::new();

    for idx in 0..side.size as u64 {
        if !index::is_canonical_slot(sub, idx) {
            continue;
        }
        let pos = index::unindex(sub, idx);
        let successors = moves_movement(pos.white(), pos.black());

        if successors.is_empty() {
            decided.push((0, idx)); // Loss(0); committed when its bucket is processed
            continue;
        }

        let mut best_win_depth: Option<u16> = None;
        let mut draw_capture_count: u32 = 0;
        let mut max_capture_win_depth: u16 = 0;
        let mut quiet_classes: std::collections::HashSet<(SubspaceId, u64)> = std::collections::HashSet::new();

        for succ in &successors {
            if succ.white_count() < 3 {
                best_win_depth = Some(1);
                break;
            }
            let succ_total = (succ.white_count() + succ.black_count()) as usize;
            if succ_total < total {
                let code = db.lookup_pos(*succ);
                if code == DRAW {
                    draw_capture_count += 1;
                } else if is_loss(code) {
                    let d = code + 1;
                    best_win_depth = Some(best_win_depth.map_or(d, |bd| bd.min(d)));
                } else {
                    // Opponent win via this capture: already satisfied
                    // (no need to wait on it), but it still contributes to
                    // this state's eventual loss depth if all other
                    // successors also turn out to be opponent wins.
                    max_capture_win_depth = max_capture_win_depth.max(code);
                }
            } else {
                quiet_classes.insert(index::index(*succ));
            }
        }

        if let Some(d) = best_win_depth {
            // Do NOT commit here. A capturing win is known instantly (the
            // smaller pair it lands in is already fully solved), but this
            // state might *also* have a quiet successor that will later
            // resolve to an even smaller-depth win via propagation — and
            // propagation only discovers that later, as it processes
            // buckets in increasing depth order. If we wrote `side.val`
            // immediately here (before any bucket processing has even
            // started), we would unconditionally pre-empt that better,
            // still-undiscovered quiet win: propagation's own guard
            // (`apply_to_predecessor`'s "already decided" check) would see
            // this state as settled and never get a chance to improve it.
            // Deferring — pushing a *candidate* into bucket `d` and only
            // committing it if the slot is still undecided when that
            // bucket is actually reached — lets a genuinely smaller-depth
            // quiet win (processed in an earlier bucket, by construction)
            // win the race instead.
            decided.push((d, idx));
            continue;
        }

        // `count` must equal the number of decrement *events* this state
        // will actually receive during propagation, not the raw successor
        // count. Those aren't the same when this state's own symmetry
        // stabilizer is nontrivial: several raw successors can collapse
        // into the same canonical class, but that class only ever gets
        // decided once, triggering exactly one `quiet_predecessors` call
        // on its canonical representative. The reciprocal multiplicity
        // back to this exact slot depends on *that* class's own
        // stabilizer too (an orbit-counting ratio, not simply 1 or the
        // raw count) — rather than derive the closed form, we compute it
        // directly per distinct class by actually running
        // `quiet_predecessors` and counting hits back to this slot. This
        // is the same computation propagation will perform later, just
        // done once up front so `count` is exact from the start.
        let mut quiet_count: u32 = 0;
        for &(csub, cidx) in &quiet_classes {
            let class_repr = index::unindex(csub, cidx);
            quiet_count += quiet_predecessors(class_repr)
                .iter()
                .filter(|p| index::index(**p) == (sub, idx))
                .count() as u32;
        }

        let count = quiet_count + draw_capture_count;
        // Only take the immediate-Loss fast path when there are truly no
        // quiet successors at all (`quiet_classes.is_empty()`) — not just
        // when the computed `count` happens to be 0. The two should
        // always coincide (a nonempty quiet class is proven, by the
        // orbit-counting argument in the comment above, to always
        // contribute at least one reciprocal hit), but this state is
        // decided as *final* here — bypassing the deferred-commit
        // mechanism that protects every other win/loss decision in this
        // function — so it must not depend on that argument holding with
        // no double-check. Loss(0)/no-quiet-successors is the only truly
        // race-free case: with nothing left to resolve, there is no
        // competing path that could ever produce a different value.
        if count == 0 && quiet_classes.is_empty() {
            let d = max_capture_win_depth + 1;
            decided.push((d, idx)); // race-free, but deferred uniformly with everything else
            continue;
        }

        side.count[idx as usize] = count;
        side.max_seen_depth[idx as usize] = max_capture_win_depth;
    }

    decided
}

/// Apply a just-decided state's consequences to one predecessor side.
/// `loss` is whether the decided state (at `depth`) is a loss for its
/// mover. Returns `Some((new_depth, idx))` if this predecessor became
/// decided as a result.
fn apply_to_predecessor(pred_side: &mut Side, pidx: u64, depth: u16, loss: bool) -> Option<(u16, u64)> {
    let pi = pidx as usize;
    if pred_side.val[pi] != DRAW {
        return None; // already decided
    }
    if loss {
        pred_side.val[pi] = depth + 1;
        Some((depth + 1, pidx))
    } else {
        pred_side.max_seen_depth[pi] = pred_side.max_seen_depth[pi].max(depth);
        debug_assert!(pred_side.count[pi] > 0, "count underflow");
        pred_side.count[pi] -= 1;
        if pred_side.count[pi] == 0 {
            let d = pred_side.max_seen_depth[pi] + 1;
            pred_side.val[pi] = d;
            Some((d, pidx))
        } else {
            None
        }
    }
}

/// Try to commit a bucket entry at depth `d` to `val[idx]`, and report
/// whether its predecessors should be processed.
///
/// Two kinds of bucket entries reach this function: genuine
/// propagation-discovered decisions (already committed to exactly `d` at
/// push time, by `apply_to_predecessor`) and deferred capture-based win
/// candidates from `init_side` (not committed yet, since they might be
/// beaten by a smaller-depth quiet win processed in an earlier bucket).
/// Both are handled uniformly here: if the slot is still undecided, this
/// is the earliest bucket that reached it, so it wins — commit and
/// process. If it already holds exactly `d`, it's the push-time commit of
/// a genuine decision — process (no-op re-commit). Otherwise it was
/// already decided at a smaller depth by something processed earlier;
/// this entry is stale and must be skipped, including skipping
/// predecessor processing (already done when the real decision landed).
fn should_process(val: &mut [u16], idx: u64, d: u16) -> bool {
    let pi = idx as usize;
    if val[pi] == DRAW {
        val[pi] = d;
        true
    } else {
        val[pi] == d
    }
}

/// Solve the unordered pair `{a, b}`. All strictly-smaller pairs (by
/// total stone count) must already be registered in `db`.
pub fn solve_pair(a: usize, b: usize, db: &Database) -> PairResult {
    if a == b {
        let val = solve_self_paired(a, db);
        return PairResult {
            a,
            b,
            val_ab: val.clone(),
            val_ba: val,
        };
    }

    let mut side_a = Side::new(a, b); // subspace (a,b)
    let mut side_b = Side::new(b, a); // subspace (b,a)

    let mut buckets: Vec<Vec<(bool, u64)>> = Vec::new(); // (is_side_b, idx)
    let push2 = |buckets: &mut Vec<Vec<(bool, u64)>>, depth: u16, is_b: bool, idx: u64| {
        let d = depth as usize;
        if buckets.len() <= d {
            buckets.resize_with(d + 1, Vec::new);
        }
        buckets[d].push((is_b, idx));
    };

    for (d, idx) in init_side(&mut side_a, db) {
        push2(&mut buckets, d, false, idx);
    }
    for (d, idx) in init_side(&mut side_b, db) {
        push2(&mut buckets, d, true, idx);
    }

    let mut d = 0usize;
    while d < buckets.len() {
        let items = std::mem::take(&mut buckets[d]);
        for (is_b, idx) in items {
            let side_val = if !is_b { &mut side_a.val } else { &mut side_b.val };
            if !should_process(side_val, idx, d as u16) {
                continue;
            }
            let sub = if !is_b {
                SubspaceId::new(side_a.w, side_a.b)
            } else {
                SubspaceId::new(side_b.w, side_b.b)
            };
            let pos = index::unindex(sub, idx);
            let loss = (d % 2) == 0;

            for pred in quiet_predecessors(pos) {
                let (psub, pidx) = index::index(pred);
                let pred_on_a = psub.w as usize == side_a.w && psub.b as usize == side_a.b;
                let pred_on_b = psub.w as usize == side_b.w && psub.b as usize == side_b.b;
                debug_assert!(pred_on_a ^ pred_on_b, "predecessor must land in exactly one side");

                let target = if pred_on_a { &mut side_a } else { &mut side_b };
                if let Some((nd, nidx)) = apply_to_predecessor(target, pidx, d as u16, loss) {
                    push2(&mut buckets, nd, pred_on_b, nidx);
                }
            }
        }
        d += 1;
    }

    PairResult {
        a,
        b,
        val_ab: side_a.val,
        val_ba: side_b.val,
    }
}

/// Self-paired case (`a == b`): subspaces `N(a,a)` and `N(a,a)` coincide,
/// so there is exactly one array and every quiet predecessor routes back
/// into it.
fn solve_self_paired(a: usize, db: &Database) -> Vec<u16> {
    let mut side = Side::new(a, a);
    let mut buckets: Vec<Vec<u64>> = Vec::new();

    for (d, idx) in init_side(&mut side, db) {
        push_bucket(&mut buckets, d, idx);
    }

    let mut d = 0usize;
    while d < buckets.len() {
        let items = std::mem::take(&mut buckets[d]);
        for idx in items {
            if !should_process(&mut side.val, idx, d as u16) {
                continue;
            }
            let sub = SubspaceId::new(side.w, side.b);
            let pos = index::unindex(sub, idx);
            let loss = (d % 2) == 0;
            for pred in quiet_predecessors(pos) {
                let (_psub, pidx) = index::index(pred);
                if let Some((nd, nidx)) = apply_to_predecessor(&mut side, pidx, d as u16, loss) {
                    push_bucket(&mut buckets, nd, nidx);
                }
            }
        }
        d += 1;
    }

    side.val
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle;

    fn to_oracle_value(code: u16) -> oracle::Value {
        if code == DRAW {
            oracle::Value::Draw
        } else if code % 2 == 0 {
            oracle::Value::Loss(code as u32)
        } else {
            oracle::Value::Win(code as u32)
        }
    }

    fn diff_against_oracle(sub: SubspaceId, values: &[u16], oracle_values: &HashMap<Position, oracle::Value>) -> Vec<(Position, oracle::Value, oracle::Value)> {
        let mut mismatches = Vec::new();
        for idx in 0..values.len() as u64 {
            if !index::is_canonical_slot(sub, idx) {
                continue;
            }
            let pos = index::unindex(sub, idx);
            let ours = to_oracle_value(values[idx as usize]);
            let theirs = *oracle_values
                .get(&pos)
                .unwrap_or_else(|| panic!("oracle missing raw state {pos:?}"));
            if ours != theirs {
                mismatches.push((pos, ours, theirs));
            }
        }
        mismatches
    }

    #[test]
    fn solve_3_3_matches_oracle_exactly() {
        let db = Database::new();
        let result = solve_pair(3, 3, &db);
        assert_eq!(result.a, 3);
        assert_eq!(result.b, 3);

        let oracle_values = oracle::solve_pair(3, 3, &HashMap::new());
        let sub = SubspaceId::new(3, 3);
        let mismatches = diff_against_oracle(sub, &result.val_ab, &oracle_values);
        assert!(
            mismatches.is_empty(),
            "{} mismatches, first few: {:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(10)]
        );
        // val_ba must be identical to val_ab in the self-paired case.
        assert_eq!(result.val_ab, result.val_ba);
    }

    #[test]
    fn solve_4_3_matches_oracle_exactly() {
        let mut db = Database::new();
        let r33 = solve_pair(3, 3, &db);
        db.insert(3, 3, r33.val_ab);

        let result = solve_pair(4, 3, &db);
        // oracle::solve_pair(4, 3, ..) jointly solves both ordered
        // subspaces (4,3) and (3,4) together (required: a quiet move flips
        // perspective between them), so the same map checks both. It also
        // needs the already-solved {3,3} pair's raw values (keyed by
        // Position, same oracle representation retro.rs's val_ab uses
        // conceptually, just unreduced) for captures that land there.
        let oracle_33 = oracle::solve_pair(3, 3, &HashMap::new());
        let oracle_values = oracle::solve_pair(4, 3, &oracle_33);

        let sub_43 = SubspaceId::new(4, 3);
        let mismatches = diff_against_oracle(sub_43, &result.val_ab, &oracle_values);
        assert!(
            mismatches.is_empty(),
            "(4,3) subspace: {} mismatches, first few: {:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(10)]
        );

        let sub_34 = SubspaceId::new(3, 4);
        let mismatches_34 = diff_against_oracle(sub_34, &result.val_ba, &oracle_values);
        assert!(
            mismatches_34.is_empty(),
            "(3,4) subspace: {} mismatches, first few: {:?}",
            mismatches_34.len(),
            &mismatches_34[..mismatches_34.len().min(10)]
        );
    }

    #[test]
    fn deterministic_rerun_is_byte_identical() {
        let db = Database::new();
        let r1 = solve_pair(3, 3, &db);
        let r2 = solve_pair(3, 3, &db);
        assert_eq!(r1.val_ab, r2.val_ab);
    }
}
