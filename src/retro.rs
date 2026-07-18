//! Retrograde analysis engine, parallelized with rayon.
//!
//! ## Concurrency model
//!
//! `Side`'s three per-state arrays (`val`, `count`, `max_seen_depth`) use
//! atomics so `init_side` can process all slots of a subspace in parallel
//! (each slot is written by exactly one task, so there's no true
//! contention there) and so a single bucket's items can be processed in
//! parallel during propagation (where a predecessor slot genuinely *can*
//! be touched by more than one concurrent task — e.g. two different items
//! in the same bucket both having it as a quiet predecessor). Every
//! atomic operation here uses `SeqCst`. That's stronger (and slower) than
//! strictly necessary, but the alternative is reasoning precisely about
//! which weaker orderings are safe for a fairly intricate multi-variable
//! protocol (a `count` fetch_sub reaching zero must be guaranteed to
//! observe *every* concurrent `max_seen_depth` update that happened
//! first, or the computed loss depth could be wrong) — over a
//! computation spanning billions of states, a silent ordering bug would
//! be far more costly than the fixed per-operation cost of a full fence.
//! `compare_exchange` is used everywhere a slot transitions from
//! undecided to decided, so concurrent writers to the same slot always
//! have exactly one winner and the rest observe the already-decided
//! value and back off — never two writers racing to completion.
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
//!   course of propagation). When `count` reaches 0, the resulting loss
//!   depth (`max_seen_depth + 1`) is correct only because bucket-order
//!   processing guarantees the last successor accounted for has the
//!   maximum depth among all of them.

use crate::index::{self, SubspaceId};
use crate::movegen::{moves_movement, quiet_predecessors};
use crate::pos::Position;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering::SeqCst};

pub const DRAW: u16 = u16::MAX;

#[inline]
pub fn is_loss(code: u16) -> bool {
    code != DRAW && code.is_multiple_of(2)
}

#[inline]
pub fn is_win(code: u16) -> bool {
    code != DRAW && code % 2 == 1
}

/// One subspace's backing storage: either fully resident (used by the
/// retrograde solver itself, whose per-pair working set is small — at
/// most two dependency subspaces at a time, see persist.rs's module
/// docs) or memory-mapped (used by callers like the opening search that
/// may need many or all subspaces at once, where holding everything as
/// owned `Vec`s would risk exhausting RAM — the full database is on the
/// order of the machine's total RAM).
enum Backing {
    Owned(Vec<u16>),
    Mapped(memmap2::Mmap),
}

impl Backing {
    #[inline]
    fn get(&self, idx: u64) -> u16 {
        match self {
            Backing::Owned(v) => v[idx as usize],
            Backing::Mapped(m) => crate::persist::mmap_get_u16(m, idx),
        }
    }
}

/// A registry of fully-solved subspaces, used for cross-pair capture
/// lookups (always into strictly smaller pairs, per the solve DAG).
#[derive(Default)]
pub struct Database {
    arrays: HashMap<(u8, u8), Backing>,
}

impl Database {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, w: usize, b: usize, values: Vec<u16>) {
        self.arrays.insert((w as u8, b as u8), Backing::Owned(values));
    }

    pub fn insert_mmap(&mut self, w: usize, b: usize, mmap: memmap2::Mmap) {
        self.arrays.insert((w as u8, b as u8), Backing::Mapped(mmap));
    }

    pub fn has(&self, w: usize, b: usize) -> bool {
        self.arrays.contains_key(&(w as u8, b as u8))
    }

    pub fn get(&self, sub: SubspaceId, idx: u64) -> u16 {
        self.arrays[&(sub.w, sub.b)].get(idx)
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

/// One ordered subspace's mutable solving state. Uses interior mutability
/// (atomics) throughout — see the module-level concurrency notes — so
/// `&Side` is enough for both parallel init and parallel propagation;
/// nothing here ever needs `&mut Side`.
struct Side {
    w: usize,
    b: usize,
    size: usize,
    val: Vec<AtomicU16>,
    count: Vec<AtomicU32>,
    max_seen_depth: Vec<AtomicU16>,
}

impl Side {
    fn new(w: usize, b: usize) -> Self {
        let size = index::subspace_size(SubspaceId::new(w, b)) as usize;
        Side {
            w,
            b,
            size,
            val: (0..size).into_par_iter().map(|_| AtomicU16::new(DRAW)).collect(),
            count: (0..size).into_par_iter().map(|_| AtomicU32::new(0)).collect(),
            max_seen_depth: (0..size).into_par_iter().map(|_| AtomicU16::new(0)).collect(),
        }
    }

    fn snapshot_val(&self) -> Vec<u16> {
        self.val.par_iter().map(|x| x.load(SeqCst)).collect()
    }
}

/// A bucket entry. `committed` distinguishes the two ways an entry can
/// arrive (see `should_process`'s docs for why this matters):
/// `true` — a propagation-discovered decision, already committed to
/// `val[idx]` via CAS at push time (in `apply_to_predecessor`); process
/// its predecessors unconditionally, no further check needed, since that
/// CAS already proved uniqueness. `false` — a tentative capture-based
/// win candidate from `init_side`, not yet committed; must go through
/// `should_process`'s strict CAS at pop time, and may lose that race.
fn push_bucket(buckets: &mut Vec<Vec<(u64, bool)>>, depth: u16, idx: u64, committed: bool) {
    let d = depth as usize;
    if buckets.len() <= d {
        buckets.resize_with(d + 1, Vec::new);
    }
    buckets[d].push((idx, committed));
}

/// Initialize one ordered subspace: mark terminal losses (no moves),
/// terminal wins (a capture reduces the opponent below 3 stones), and
/// resolve every capturing successor against the (already-solved)
/// smaller pair it lands in, immediately deciding any state whose
/// successors are already fully accounted for. Everything else gets a
/// `count`/`max_seen_depth` scratch entry and stays a pending draw
/// candidate until propagation resolves it (or leaves it a true draw).
/// Returns the list of (depth, idx) pairs decided during this pass.
/// Parallel over slots: each slot is written by exactly one task (either
/// contributing an entry to the returned list, or storing its own
/// `count`/`max_seen_depth`), so there is no contention within this
/// function despite the shared `&Side`.
fn init_side(side: &Side, db: &Database) -> Vec<(u16, u64)> {
    let sub = SubspaceId::new(side.w, side.b);
    let total = side.w + side.b;

    (0..side.size as u64)
        .into_par_iter()
        .filter_map(|idx| {
        if !index::is_canonical_slot(sub, idx) {
            return None;
        }
        let pos = index::unindex(sub, idx);
        let successors = moves_movement(pos.white(), pos.black());

        if successors.is_empty() {
            return Some((0, idx)); // Loss(0); committed when its bucket is processed
        }

        let mut best_win_depth: Option<u16> = None;
        let mut draw_capture_count: u32 = 0;
        let mut max_capture_win_depth: u16 = 0;
        // Distinct canonical quiet-successor classes, each mapped to `f`:
        // how many of this state's *raw* successors land in that class
        // (needed by the analytical reciprocal-count formula below).
        let mut quiet_classes: std::collections::HashMap<(SubspaceId, u64), u32> = std::collections::HashMap::new();

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
                *quiet_classes.entry(index::index(*succ)).or_insert(0) += 1;
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
            //
            // `count` is intentionally left unset here rather than
            // computed exactly (we `break`-out of the successor scan
            // above as soon as a capture win is found, so `quiet_classes`
            // may be incomplete, and this state's own eventual value
            // never actually depends on `count` — it already has a
            // legitimate win). But this slot can still be *targeted* by
            // some other, unrelated state's quiet-predecessor scan (if a
            // sibling quiet successor of this state independently becomes
            // a win, `apply_to_predecessor` will try to decrement this
            // slot's count) — so it still needs a value that safely
            // absorbs that, rather than the default 0, which would
            // decrement into a wraparound and, in a debug build,
            // trip the underflow assertion. Sentinel it to a value no
            // realistic number of decrements can ever bring to exactly 0
            // (branching factors here never remotely approach `u32::MAX`,
            // so this can never spuriously flip the slot to a wrong Loss).
            side.count[idx as usize].store(u32::MAX, SeqCst);
            return Some((d, idx));
        }

        // `count` must equal the number of decrement *events* this state
        // will actually receive during propagation, not the raw successor
        // count. Those aren't the same when this state's own symmetry
        // stabilizer is nontrivial: several raw successors can collapse
        // into the same canonical class, but that class only ever gets
        // decided once, triggering exactly one predecessor-discovery
        // event. The reciprocal multiplicity back to this exact slot is
        // an orbit-counting ratio, computed analytically rather than by
        // re-simulating `quiet_predecessors` per class (which cost
        // O(branching factor) per class and dominated init's runtime on
        // high-branching, jump-heavy pairs):
        //
        //   count(a,b pairs with a-->b, a in orbit(P), b in orbit(C))
        //     = |orbit(P)| * f(P,C) = |orbit(C)| * r(C,P)
        //   => r(C,P) = f(P,C) * |orbit(P)| / |orbit(C)|
        //             = f(P,C) * stab(C) / stab(P)
        //
        // (|orbit(X)| = 16 / stab(X) since the 16 symmetries form a group
        // acting transitively on each orbit — Lagrange's theorem, checked
        // directly by a property test in symmetry.rs). `f` is this
        // state's own raw successor count into class C, already tallied
        // above; `stab(P)`/`stab(C)` are cheap (a 16-way loop each),
        // independent of branching factor. Verified byte-for-byte
        // identical to the direct-simulation approach it replaced, and
        // independently re-verified end-to-end against the oracle.
        let stab_p = crate::symmetry::stabilizer_size(pos);
        let mut quiet_count: u32 = 0;
        for (&(csub, cidx), &f) in &quiet_classes {
            let class_repr = index::unindex(csub, cidx);
            let stab_c = crate::symmetry::stabilizer_size(class_repr);
            let numerator = f as usize * stab_c;
            debug_assert_eq!(
                numerator % stab_p,
                0,
                "non-integer reciprocal multiplicity: f={f} stab_c={stab_c} stab_p={stab_p}"
            );
            quiet_count += (numerator / stab_p) as u32;
        }

        // Debug-only regression guard: re-derive the same count by direct
        // simulation (the approach this formula replaced) and compare.
        // Zero cost in release builds. This exact computation has already
        // hidden two real bugs once (see git history) — cheap insurance
        // against a future change silently breaking the orbit-counting
        // argument above for some case this session's testing didn't hit.
        #[cfg(debug_assertions)]
        {
            let mut sim_count: u32 = 0;
            for &(csub, cidx) in quiet_classes.keys() {
                let class_repr = index::unindex(csub, cidx);
                sim_count += quiet_predecessors(class_repr)
                    .iter()
                    .filter(|p| index::index(**p) == (sub, idx))
                    .count() as u32;
            }
            debug_assert_eq!(
                quiet_count, sim_count,
                "reciprocal-count formula/simulation mismatch at idx={idx} pos white={:?} black={:?} classes={:?}",
                pos.white(),
                pos.black(),
                quiet_classes
            );
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
            return Some((d, idx)); // race-free, but deferred uniformly with everything else
        }

        side.count[idx as usize].store(count, SeqCst);
        side.max_seen_depth[idx as usize].store(max_capture_win_depth, SeqCst);
        None
        })
        .collect()
}

/// Apply a just-decided state's consequences to one predecessor side.
/// `loss` is whether the decided state (at `depth`) is a loss for its
/// mover. Returns `Some((new_depth, idx))` if this predecessor became
/// decided as a result. Safe under concurrent calls targeting the same
/// `pidx` (from other items in the same bucket): the final `val` write
/// (whether the immediate win-commit or the count-reaches-zero
/// loss-commit) is a `compare_exchange` from `DRAW`, so only one
/// concurrent caller ever wins the transition; the rest observe the
/// already-decided value and return `None`. `count`/`max_seen_depth`
/// updates are lock-free RMWs, safe under any number of concurrent
/// decrementers.
fn apply_to_predecessor(pred_side: &Side, pidx: u64, depth: u16, loss: bool) -> Option<(u16, u64)> {
    let pi = pidx as usize;
    if loss {
        match pred_side.val[pi].compare_exchange(DRAW, depth + 1, SeqCst, SeqCst) {
            Ok(_) => Some((depth + 1, pidx)),
            Err(_) => None, // already decided (necessarily at <= this depth)
        }
    } else {
        pred_side.max_seen_depth[pi].fetch_max(depth, SeqCst);
        let prev = pred_side.count[pi].fetch_sub(1, SeqCst);
        debug_assert!(prev > 0, "count underflow");
        if prev == 1 {
            // This call brought count to 0. SeqCst on both the
            // max_seen_depth store above and this fetch_sub (and every
            // other thread's matching pair of operations) guarantees this
            // load observes every concurrent max_seen_depth update that
            // happened before its corresponding count decrement — see the
            // module-level concurrency notes.
            let d = pred_side.max_seen_depth[pi].load(SeqCst) + 1;
            match pred_side.val[pi].compare_exchange(DRAW, d, SeqCst, SeqCst) {
                Ok(_) => Some((d, pidx)),
                Err(_) => None,
            }
        } else {
            None
        }
    }
}

/// Try to commit a *tentative* (not-yet-committed) bucket entry at depth
/// `d` to `val[idx]`, and report whether its predecessors should be
/// processed.
///
/// Only `init_side`'s deferred capture-based win candidates are tentative
/// — see the module docs on the two kinds of bucket entries. This is a
/// strict CAS: succeed only if the slot is still undecided. It must NOT
/// also accept "already holds exactly `d`" as success. That looks
/// harmless (as if re-confirming the same decision) but isn't: a
/// tentative entry and a genuine propagation-committed entry for the same
/// slot can coexist in the same bucket when they happen to land on the
/// same depth (the committed one's CAS already ran, at push time, during
/// the *previous* bucket's processing — strictly before this bucket
/// starts). If a tentative entry that lost that race were still allowed
/// to "process" just because the value matches, its predecessors would
/// be visited a second time, double-decrementing their `count` — this
/// was a real bug (see git history) caught by the forward-consistency
/// verifier on a pair large enough for a capture-based tentative win and
/// a same-depth quiet win to coincide, something the small pairs used
/// for oracle cross-checks never happened to exercise.
fn should_process(slot: &AtomicU16, d: u16) -> bool {
    slot.compare_exchange(DRAW, d, SeqCst, SeqCst).is_ok()
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

    let side_a = Side::new(a, b); // subspace (a,b)
    let side_b = Side::new(b, a); // subspace (b,a)

    // (is_side_b, idx, committed) — see `push_bucket`'s docs for what
    // `committed` means.
    let mut buckets: Vec<Vec<(bool, u64, bool)>> = Vec::new();
    let push2 = |buckets: &mut Vec<Vec<(bool, u64, bool)>>, depth: u16, is_b: bool, idx: u64, committed: bool| {
        let d = depth as usize;
        if buckets.len() <= d {
            buckets.resize_with(d + 1, Vec::new);
        }
        buckets[d].push((is_b, idx, committed));
    };

    for (d, idx) in init_side(&side_a, db) {
        push2(&mut buckets, d, false, idx, false);
    }
    for (d, idx) in init_side(&side_b, db) {
        push2(&mut buckets, d, true, idx, false);
    }

    // Buckets are processed strictly in increasing depth order (a hard
    // sequential dependency: bucket d+1 can only be correctly populated
    // once bucket d is fully resolved). Within one bucket, all items are
    // independent of each other except for possibly sharing a predecessor
    // slot, which `apply_to_predecessor`/`should_process` handle safely
    // under concurrent access — so each bucket's items are processed in
    // parallel via rayon, and the resulting newly-decided predecessors
    // are collected and pushed into later buckets sequentially afterward.
    let mut d = 0usize;
    while d < buckets.len() {
        let items = std::mem::take(&mut buckets[d]);
        let new_items: Vec<(bool, u64, u16)> = items
            .into_par_iter()
            .flat_map_iter(|(is_b, idx, committed)| {
                let side = if !is_b { &side_a } else { &side_b };
                let mut out = Vec::new();
                if !committed && !should_process(&side.val[idx as usize], d as u16) {
                    return out.into_iter();
                }
                let sub = SubspaceId::new(side.w, side.b);
                let pos = index::unindex(sub, idx);
                let loss = d.is_multiple_of(2);

                for pred in quiet_predecessors(pos) {
                    let (psub, pidx) = index::index(pred);
                    let pred_on_a = psub.w as usize == side_a.w && psub.b as usize == side_a.b;
                    let pred_on_b = psub.w as usize == side_b.w && psub.b as usize == side_b.b;
                    debug_assert!(pred_on_a ^ pred_on_b, "predecessor must land in exactly one side");

                    let target = if pred_on_a { &side_a } else { &side_b };
                    if let Some((nd, nidx)) = apply_to_predecessor(target, pidx, d as u16, loss) {
                        out.push((pred_on_b, nidx, nd));
                    }
                }
                out.into_iter()
            })
            .collect();
        for (is_b, idx, nd) in new_items {
            push2(&mut buckets, nd, is_b, idx, true);
        }
        d += 1;
    }

    PairResult {
        a,
        b,
        val_ab: side_a.snapshot_val(),
        val_ba: side_b.snapshot_val(),
    }
}

/// Self-paired case (`a == b`): subspaces `N(a,a)` and `N(a,a)` coincide,
/// so there is exactly one array and every quiet predecessor routes back
/// into it.
fn solve_self_paired(a: usize, db: &Database) -> Vec<u16> {
    let side = Side::new(a, a);
    let mut buckets: Vec<Vec<(u64, bool)>> = Vec::new();

    for (d, idx) in init_side(&side, db) {
        push_bucket(&mut buckets, d, idx, false);
    }

    let mut d = 0usize;
    while d < buckets.len() {
        let items = std::mem::take(&mut buckets[d]);
        let new_items: Vec<(u64, u16)> = items
            .into_par_iter()
            .flat_map_iter(|(idx, committed)| {
                let mut out = Vec::new();
                if !committed && !should_process(&side.val[idx as usize], d as u16) {
                    return out.into_iter();
                }
                let sub = SubspaceId::new(side.w, side.b);
                let pos = index::unindex(sub, idx);
                let loss = d.is_multiple_of(2);
                for pred in quiet_predecessors(pos) {
                    let (_psub, pidx) = index::index(pred);
                    if let Some((nd, nidx)) = apply_to_predecessor(&side, pidx, d as u16, loss) {
                        out.push((nidx, nd));
                    }
                }
                out.into_iter()
            })
            .collect();
        for (idx, nd) in new_items {
            push_bucket(&mut buckets, nd, idx, true);
        }
        d += 1;
    }

    side.snapshot_val()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle;

    fn to_oracle_value(code: u16) -> oracle::Value {
        if code == DRAW {
            oracle::Value::Draw
        } else if code.is_multiple_of(2) {
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

    /// Same check on the {4,3} pair specifically: it exercises the a != b
    /// path (two distinct concurrently-solved sides, cross-referencing
    /// each other's predecessors within a bucket) and has enough states
    /// per bucket to make race conditions in the parallel commit protocol
    /// far more likely to surface as run-to-run nondeterminism than the
    /// much smaller {3,3} pair would.
    #[test]
    fn deterministic_rerun_4_3_is_byte_identical() {
        let mut db = Database::new();
        let r33 = solve_pair(3, 3, &db);
        db.insert(3, 3, r33.val_ab);

        let r1 = solve_pair(4, 3, &db);
        let r2 = solve_pair(4, 3, &db);
        assert_eq!(r1.val_ab, r2.val_ab);
        assert_eq!(r1.val_ba, r2.val_ba);
    }
}
