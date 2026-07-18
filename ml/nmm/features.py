"""Input feature vector and the deterministic train/val/test split.

Shared by `dataset.py` (training), `evaluate.py`, and `export.py` (the golden
vectors and the feature spec written into `model.json` must exactly match what
training used). See `design-nn.md` §§3, 6.
"""

from __future__ import annotations

import numpy as np

from .board import popcount

FEATURE_DIM = 52

# Splitmix64 constants (public-domain mixing function) -- any fixed, decent
# 64-bit hash works here; splitmix64 is simple, fast, and has no external
# dependency.
_SM64_INC = np.uint64(0x9E3779B97F4A7C15)
_SM64_M1 = np.uint64(0xBF58476D1CE4E5B9)
_SM64_M2 = np.uint64(0x94D049BB133111EB)

TRAIN, VAL, TEST = 0, 1, 2


def _splitmix64(x: np.ndarray) -> np.ndarray:
    z = (x.astype(np.uint64)) + _SM64_INC
    z = (z ^ (z >> np.uint64(30))) * _SM64_M1
    z = (z ^ (z >> np.uint64(27))) * _SM64_M2
    z = z ^ (z >> np.uint64(31))
    return z


def assign_split(
    w: int, b: int, idx: np.ndarray, val_frac: float = 0.005, test_frac: float = 0.005
) -> np.ndarray:
    """Deterministic split membership (TRAIN/VAL/TEST) for slots of subspace
    (w, b), keyed only by (w, b, idx) -- reproducible without storing anything,
    and stable across re-runs / different sampling orders.
    """
    idx = np.asarray(idx, dtype=np.uint64)
    key = (np.uint64(w) << np.uint64(56)) | (np.uint64(b) << np.uint64(48)) | idx
    h = _splitmix64(key)
    u = (h >> np.uint64(11)).astype(np.float64) / float(1 << 53)
    return np.where(u < val_frac, VAL, np.where(u < val_frac + test_frac, TEST, TRAIN))


def featurize(mover: np.ndarray, opp: np.ndarray) -> np.ndarray:
    """(mover, opp) uint32 arrays -> float32[n, 52] feature matrix.

    Layout (design-nn.md §3): bits 0-23 mover occupancy, 24-47 opponent
    occupancy, 48 mover_count/9, 49 opponent_count/9, 50 mover-has-3-stones,
    51 opponent-has-3-stones. This exact layout is load-bearing: `export.py`
    and the TS runtime must reproduce it bit-for-bit.
    """
    mover = np.asarray(mover, dtype=np.uint32)
    opp = np.asarray(opp, dtype=np.uint32)
    n = mover.shape[0]
    feat = np.zeros((n, FEATURE_DIM), dtype=np.float32)
    for p in range(24):
        feat[:, p] = ((mover >> np.uint32(p)) & np.uint32(1)).astype(np.float32)
        feat[:, 24 + p] = ((opp >> np.uint32(p)) & np.uint32(1)).astype(np.float32)
    wc = popcount(mover)
    bc = popcount(opp)
    feat[:, 48] = wc.astype(np.float32) / 9.0
    feat[:, 49] = bc.astype(np.float32) / 9.0
    feat[:, 50] = (wc == 3).astype(np.float32)
    feat[:, 51] = (bc == 3).astype(np.float32)
    return feat
