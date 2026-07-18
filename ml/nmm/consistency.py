"""Forward-consistency spot check: the make-or-break gate for the Python port.

Independent of both the Rust solver *and* the Rust verify pass: for a sampled
canonical, real (non-wasted) state, recompute its WDL class and depth directly
from its successors' *stored* values (single-pass minimax, `readme-database.md`
§5's move-selection rule read as a value-consistency equation) and compare with
the value actually on disk. Mirrors the idea in `src/verify.rs` / `design.md`
§8.1, at spot-check rather than full-scan scale.

If this passes at a near-zero mismatch rate over a large enough sample, then
`board.py`, `symmetry.py`, `ranking.py`, `db.py`, and `movegen.py` are all very
likely correct *together* -- geometry, canonicalization, indexing, decoding, and
move generation all have to agree with the Rust implementation simultaneously
for a state's expected value to come out right from first principles.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np

from . import movegen, ranking, symmetry
from .board import popcount
from .db import DRAW_CLASS, LOSS, WIN, Database


@dataclass
class Mismatch:
    w: int
    b: int
    idx: int
    stored_class: int
    stored_depth: int
    expected_class: int
    expected_depth: int


@dataclass
class ConsistencyReport:
    checked: int = 0
    mismatches: list[Mismatch] = field(default_factory=list)
    per_pair_checked: dict = field(default_factory=dict)
    per_pair_mismatches: dict = field(default_factory=dict)

    @property
    def mismatch_rate(self) -> float:
        return len(self.mismatches) / self.checked if self.checked else 0.0


def expected_value(mover: int, opp: int, db: Database) -> tuple[int, int]:
    """Recompute (wdl_class, depth) for (mover, opp) from its successors'
    stored values -- the independent-recomputation side of the gate."""
    moves = movegen.moves_movement(mover, opp)
    if not moves:
        return LOSS, 0

    best_win_depth: int | None = None
    all_win = True
    max_loss_seen = 0

    for mv in moves:
        succ_mover, succ_opp = mv.successor_mover, mv.successor_opp
        if bin(succ_mover).count("1") < 3:
            cls, depth = LOSS, 0
        else:
            cls, depth = db.lookup(succ_mover, succ_opp)

        if cls == DRAW_CLASS:
            all_win = False
        elif cls == LOSS:
            d = depth + 1
            best_win_depth = d if best_win_depth is None else min(best_win_depth, d)
        else:  # WIN for the successor's mover -- bad for us unless all are
            max_loss_seen = max(max_loss_seen, depth)

    if best_win_depth is not None:
        return WIN, best_win_depth
    if all_win:
        return LOSS, max_loss_seen + 1
    return DRAW_CLASS, 0


def spot_check(
    db: Database,
    n_samples: int,
    seed: int = 0,
    max_mismatches_recorded: int = 50,
) -> ConsistencyReport:
    """Sample `n_samples` canonical states spread across all available
    subspaces (weighted by subspace size) and check forward consistency.
    """
    rng = np.random.default_rng(seed)
    pairs = db.available_pairs
    sizes = np.array([db.size(w, b) for (w, b) in pairs], dtype=np.float64)
    probs = sizes / sizes.sum()

    report = ConsistencyReport()
    # Draw more than n_samples since wasted (non-canonical) slots get filtered.
    draws_per_pair = rng.multinomial(int(n_samples * 1.3) + len(pairs), probs)

    for (w, b), n_draw in zip(pairs, draws_per_pair):
        if n_draw == 0:
            continue
        size = db.size(w, b)
        idx = rng.integers(0, size, size=n_draw)
        mover, opp = ranking.unindex_batch(w, b, idx)
        canon = symmetry.is_canonical_batch(mover, opp)
        mover, opp, idx = mover[canon], opp[canon], idx[canon]

        checked_pair = 0
        mismatches_pair = 0
        for i in range(len(idx)):
            m, o, ix = int(mover[i]), int(opp[i]), int(idx[i])
            raw = int(db.raw(w, b, ix))
            stored_cls, stored_depth = db.decode(np.array([raw]))
            stored_cls, stored_depth = int(stored_cls[0]), int(stored_depth[0])

            exp_cls, exp_depth = expected_value(m, o, db)

            checked_pair += 1
            report.checked += 1
            if exp_cls != stored_cls:
                mismatches_pair += 1
                if len(report.mismatches) < max_mismatches_recorded:
                    report.mismatches.append(
                        Mismatch(w, b, ix, stored_cls, stored_depth, exp_cls, exp_depth)
                    )
            if report.checked >= n_samples:
                break

        report.per_pair_checked[(w, b)] = (
            report.per_pair_checked.get((w, b), 0) + checked_pair
        )
        report.per_pair_mismatches[(w, b)] = (
            report.per_pair_mismatches.get((w, b), 0) + mismatches_pair
        )
        if report.checked >= n_samples:
            break

    return report
