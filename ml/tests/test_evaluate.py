from pathlib import Path

import pytest
import torch

from nmm.db import DRAW_CLASS, LOSS, WIN, Database
from nmm.evaluate import (
    NetEvaluator,
    choose_move_exact,
    evaluate_move_quality,
    run_soak,
    true_class_of_move,
)
from nmm.model import NmmNet

DB_DIR = Path(__file__).resolve().parent.parent.parent / "db"
HAVE_DB = (DB_DIR / "manifest.json").exists()


class PerfectStandInEvaluator(NetEvaluator):
    """A fake "model" whose evaluate_batch reads straight from the exact
    database -- used to sanity-check the evaluation harness itself: a
    perfect player must show zero blunders and a 100% optimal-class rate,
    independent of anything about NmmNet."""

    def __init__(self, db: Database):
        self.db = db
        self.tta = False
        self.device = "cpu"

    def evaluate_batch(self, movers, opps):
        import numpy as np

        n = len(movers)
        probs = np.zeros((n, 3), dtype=np.float32)
        depths = np.zeros(n, dtype=np.float32)
        for i in range(n):
            m, o = int(movers[i]), int(opps[i])
            if bin(m).count("1") < 3:
                cls, depth = LOSS, 0
            else:
                cls, depth = self.db.lookup(m, o)
            label = {LOSS: 0, DRAW_CLASS: 1, WIN: 2}[cls]
            probs[i, label] = 1.0
            depths[i] = depth / 255.0
        return probs, depths


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_perfect_evaluator_has_zero_blunders():
    db = Database(DB_DIR)
    ev = PerfectStandInEvaluator(db)
    metrics = evaluate_move_quality(db, ev, n_samples=500, seed=0)
    assert metrics.n > 0
    assert metrics.blunders == 0
    assert metrics.optimal_class == metrics.n


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_perfect_evaluator_never_loses_soak():
    db = Database(DB_DIR)
    ev = PerfectStandInEvaluator(db)
    result = run_soak(db, ev, n_games=20, seed=1, max_plies=200)
    assert result.games == 20
    assert result.model_losses_from_nonlost_start == 0


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_untrained_net_evaluator_runs_without_crashing():
    """Smoke test: a freshly initialized (untrained) net can still drive the
    full evaluation harness end-to-end (it will blunder a lot, that's fine)."""
    db = Database(DB_DIR)
    model = NmmNet.from_config("S")
    ev = NetEvaluator(model, tta=False)
    metrics = evaluate_move_quality(db, ev, n_samples=200, seed=2)
    assert metrics.n > 0
    assert 0.0 <= metrics.blunder_rate <= 1.0


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_true_class_of_move_matches_choose_move_exact_semantics():
    db = Database(DB_DIR)
    from nmm import ranking, symmetry

    w, b = db.available_pairs[0]
    for idx in range(0, 50):
        mover, opp = ranking.unindex(w, b, idx)
        if not symmetry.is_canonical(mover, opp):
            continue
        mv = choose_move_exact(db, mover, opp)
        if mv is None:
            continue
        # the exact player's chosen move must be at least as good, under
        # true_class_of_move, as every other legal move (it's the argmax).
        from nmm import movegen

        moves = movegen.moves_movement(mover, opp)
        chosen_cls = true_class_of_move(db, mover, opp, mv)
        rank = {LOSS: -1, DRAW_CLASS: 0, WIN: 1}
        for other in moves:
            other_cls = true_class_of_move(db, mover, opp, other)
            assert rank[chosen_cls] >= rank[other_cls]
