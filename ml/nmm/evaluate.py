"""Evaluation suite: move-quality metrics against the exact database.

design-nn.md §8 / implementation-nn.md N6. Move quality (blunder rate) is the
primary metric -- state-level accuracy is diagnostic only, since a state
misclassification only matters if it changes which move gets picked, and only
really matters if that changes the achievable game-theoretic outcome.
"""

from __future__ import annotations

from dataclasses import dataclass, field

import numpy as np
import torch

from . import movegen, ranking, symmetry
from .db import DRAW_CLASS, LOSS, WIN, Database
from .features import featurize
from .model import NmmNet

LABEL_LOSS, LABEL_DRAW, LABEL_WIN = 0, 1, 2


# ---------------------------------------------------------------------------
# Model-backed evaluators: raw net, TTA (16-way symmetry averaging), and a
# thin alpha-beta-free "1-ply move selection" that both share.
# ---------------------------------------------------------------------------


class NetEvaluator:
    """Wraps a trained NmmNet to answer "evaluate this position" for the
    move-selection rule (design-nn.md §2/§5): returns per-position
    (wdl_probs[3], predicted_depth) either from a single forward pass or
    averaged over all 16 symmetry images (test-time augmentation, "TTA").
    """

    def __init__(self, model: NmmNet, device: str = "cpu", tta: bool = False):
        self.model = model.to(device).eval()
        self.device = device
        self.tta = tta

    @torch.no_grad()
    def evaluate_batch(self, movers: np.ndarray, opps: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
        """(mover, opp) arrays -> (wdl_probs[n,3], depth[n]) for the side to
        move at each position (no canonicalization assumed by the caller)."""
        n = len(movers)
        if not self.tta:
            feats = featurize(movers, opps)
            x = torch.from_numpy(feats).to(self.device)
            logits, depth = self.model(x)
            probs = torch.softmax(logits, dim=-1).cpu().numpy()
            return probs, depth.cpu().numpy()

        probs_sum = np.zeros((n, 3), dtype=np.float64)
        depth_sum = np.zeros(n, dtype=np.float64)
        for k in range(symmetry.N_SYMS):
            m_k = symmetry.apply_batch(k, movers)
            o_k = symmetry.apply_batch(k, opps)
            feats = featurize(m_k, o_k)
            x = torch.from_numpy(feats).to(self.device)
            logits, depth = self.model(x)
            probs_sum += torch.softmax(logits, dim=-1).cpu().numpy()
            depth_sum += depth.cpu().numpy()
        return (probs_sum / symmetry.N_SYMS).astype(np.float32), (
            depth_sum / symmetry.N_SYMS
        ).astype(np.float32)


def choose_move_by_model(evaluator: NetEvaluator, mover: int, opp: int) -> movegen.Move | None:
    """1-ply move selection using the model's own successor evaluations
    (design-nn.md §2): enumerate legal moves, evaluate each successor from
    its own mover's perspective, and pick the move whose successor is worst
    for the opponent -- successor predicted LOSS (min depth) > DRAW > WIN
    (max depth). A capture dropping the opponent below 3 stones is an
    immediate win, handled by rule (never passed to the net).
    """
    moves = movegen.moves_movement(mover, opp)
    if not moves:
        return None

    below3 = [mv for mv in moves if bin(mv.successor_mover).count("1") < 3]
    if below3:
        return below3[0]  # immediate win by rule; any such move is optimal

    movers = np.array([mv.successor_mover for mv in moves], dtype=np.uint32)
    opps = np.array([mv.successor_opp for mv in moves], dtype=np.uint32)
    probs, depths = evaluator.evaluate_batch(movers, opps)
    pred_label = probs.argmax(axis=-1)

    best_i = None
    best_key = None  # sort key: (tier, tiebreak) where lower tier is better
    for i in range(len(moves)):
        label = pred_label[i]
        d = float(depths[i])
        if label == LABEL_LOSS:
            key = (0, d)  # prefer smallest depth among opponent-losses
        elif label == LABEL_DRAW:
            key = (1, 0.0)
        else:
            key = (2, -d)  # prefer largest depth among opponent-wins
        if best_key is None or key < best_key:
            best_key, best_i = key, i
    return moves[best_i]


def choose_move_exact(db: Database, mover: int, opp: int) -> movegen.Move | None:
    """1-ply move selection using *exact* stored values (readme-database.md
    §5) -- the ground-truth player used as the opponent in soak matches and
    as the source of "true best move" for blunder-rate scoring."""
    moves = movegen.moves_movement(mover, opp)
    if not moves:
        return None

    best_i = None
    best_key = None
    for i, mv in enumerate(moves):
        sm, so = mv.successor_mover, mv.successor_opp
        if bin(sm).count("1") < 3:
            cls, depth = LOSS, 0
        else:
            cls, depth = db.lookup(sm, so)
        if cls == LOSS:
            key = (0, depth)
        elif cls == DRAW_CLASS:
            key = (1, 0)
        else:
            key = (2, -depth)
        if best_key is None or key < best_key:
            best_key, best_i = key, i
    return moves[best_i]


def true_class_of_move(db: Database, mover: int, opp: int, mv: movegen.Move) -> int:
    """The game-theoretic WDL class (for `mover`, i.e. from mover's own
    perspective before the move) that results from playing `mv` -- used to
    score blunders. This is the *negation* of the successor's own stored
    class, since the successor position is from the opponent's perspective.
    """
    sm, so = mv.successor_mover, mv.successor_opp
    if bin(sm).count("1") < 3:
        succ_cls = LOSS  # immediate loss for successor's mover = win for us
    else:
        succ_cls, _ = db.lookup(sm, so)
    return -succ_cls  # WIN(1)<->LOSS(-1) flip; DRAW(0) stays 0


# ---------------------------------------------------------------------------
# Move-quality metrics
# ---------------------------------------------------------------------------


@dataclass
class MoveMetrics:
    n: int = 0
    blunders: int = 0
    optimal_class: int = 0  # chosen move's resulting class matches the best achievable
    optimal_depth: int = 0  # optimal_class AND (for wins) matches minimal depth
    per_pair: dict = field(default_factory=dict)  # (w,b) -> {n, blunders}

    @property
    def blunder_rate(self) -> float:
        return self.blunders / self.n if self.n else 0.0

    @property
    def optimal_class_rate(self) -> float:
        return self.optimal_class / self.n if self.n else 0.0


def evaluate_move_quality(
    db: Database,
    evaluator: NetEvaluator,
    n_samples: int,
    seed: int = 0,
) -> MoveMetrics:
    """For sampled canonical, real, non-terminal positions: compare the
    model's chosen move against the database-optimal move. A "blunder" is a
    move whose true resulting class is strictly worse than the best
    achievable class (win -> draw/loss, or draw -> loss).
    """
    rng = np.random.default_rng(seed)
    pairs = db.available_pairs
    sizes = np.array([db.size(w, b) for (w, b) in pairs], dtype=np.float64)
    probs = sizes / sizes.sum()

    metrics = MoveMetrics()
    draws_per_pair = rng.multinomial(int(n_samples * 1.3) + len(pairs), probs)

    class_rank = {LOSS: 2, DRAW_CLASS: 1, WIN: 0}  # higher is better for `mover`

    for (w, b), n_draw in zip(pairs, draws_per_pair):
        if n_draw == 0 or metrics.n >= n_samples:
            continue
        size = db.size(w, b)
        idx = rng.integers(0, size, size=n_draw)
        mover, opp = ranking.unindex_batch(w, b, idx)
        canon = symmetry.is_canonical_batch(mover, opp)
        mover, opp = mover[canon], opp[canon]

        pair_n = 0
        pair_blunders = 0
        for i in range(len(mover)):
            m, o = int(mover[i]), int(opp[i])
            moves = movegen.moves_movement(m, o)
            if not moves:
                continue  # terminal position, no move to evaluate

            model_mv = choose_move_by_model(evaluator, m, o)
            best_mv = choose_move_exact(db, m, o)

            true_cls_chosen = true_class_of_move(db, m, o, model_mv)
            true_cls_best = true_class_of_move(db, m, o, best_mv)

            metrics.n += 1
            pair_n += 1
            if class_rank[true_cls_chosen] < class_rank[true_cls_best]:
                metrics.blunders += 1
                pair_blunders += 1
            if true_cls_chosen == true_cls_best:
                metrics.optimal_class += 1

            if metrics.n >= n_samples:
                break

        prev = metrics.per_pair.get((w, b), {"n": 0, "blunders": 0})
        metrics.per_pair[(w, b)] = {
            "n": prev["n"] + pair_n,
            "blunders": prev["blunders"] + pair_blunders,
        }
        if metrics.n >= n_samples:
            break

    return metrics


# ---------------------------------------------------------------------------
# Soak matches: model stack vs. exact database player
# ---------------------------------------------------------------------------


@dataclass
class SoakResult:
    games: int = 0
    model_losses_from_nonlost_start: int = 0
    draws: int = 0
    model_wins: int = 0
    max_plies_hit: int = 0


def play_soak_match(
    db: Database,
    evaluator: NetEvaluator,
    start_mover: int,
    start_opp: int,
    model_plays_first: bool,
    max_plies: int = 400,
) -> str:
    """Play one movement-phase game, model vs. exact database player, from a
    given start. Returns 'model_win' / 'model_loss' / 'draw' (draw = hit the
    ply cap without a decisive result, standing in for repetition).
    """
    mover, opp = start_mover, start_opp
    model_to_move = model_plays_first
    for _ply in range(max_plies):
        term = movegen.terminal_class(mover, opp)
        if term is not None:
            # side to move (mover) has lost.
            mover_is_model = model_to_move
            return "model_loss" if mover_is_model else "model_win"

        if bin(mover).count("1") < 3:
            mover_is_model = model_to_move
            return "model_loss" if mover_is_model else "model_win"

        if model_to_move:
            mv = choose_move_by_model(evaluator, mover, opp)
        else:
            mv = choose_move_exact(db, mover, opp)
        mover, opp = mv.successor_mover, mv.successor_opp
        model_to_move = not model_to_move
    return "draw"


def run_soak(
    db: Database,
    evaluator: NetEvaluator,
    n_games: int,
    seed: int = 0,
    max_plies: int = 400,
) -> SoakResult:
    """Sample starting positions that are drawn-or-better for whichever side
    moves first (so a model loss is unambiguously attributable to play, not
    an already-lost start), and play them out both ways (model first / model
    second) against the exact database player.
    """
    rng = np.random.default_rng(seed)
    pairs = db.available_pairs
    sizes = np.array([db.size(w, b) for (w, b) in pairs], dtype=np.float64)
    probs = sizes / sizes.sum()

    result = SoakResult()
    attempts = 0
    max_attempts = n_games * 50 + 100
    while result.games < n_games and attempts < max_attempts:
        attempts += 1
        gi = rng.choice(len(pairs), p=probs)
        w, b = pairs[gi]
        size = db.size(w, b)
        idx = int(rng.integers(0, size))
        mover, opp = ranking.unindex(w, b, idx)
        if not symmetry.is_canonical(mover, opp):
            continue
        cls, _ = db.lookup(mover, opp)
        if cls == LOSS:
            continue  # already lost for the side to move; not a fair test

        model_first = bool(rng.integers(0, 2))
        outcome = play_soak_match(db, evaluator, mover, opp, model_first, max_plies)
        result.games += 1
        if outcome == "model_loss":
            result.model_losses_from_nonlost_start += 1
        elif outcome == "model_win":
            result.model_wins += 1
        else:
            result.draws += 1

    return result
