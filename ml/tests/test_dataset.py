import time
from pathlib import Path

import numpy as np
import pytest
import torch

from nmm import ranking, symmetry
from nmm.dataset import LABEL_DRAW, LABEL_LOSS, LABEL_WIN, NmmIterableDataset
from nmm.db import Database
from nmm.features import FEATURE_DIM

DB_DIR = Path(__file__).resolve().parent.parent.parent / "db"
HAVE_DB = (DB_DIR / "manifest.json").exists()


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_label_invariant_under_random_symmetry():
    """The whole point of augmentation: db.lookup (hence the training label)
    is unchanged by applying any of the 16 symmetries to a position."""
    db = Database(DB_DIR)
    rng = np.random.default_rng(0)
    (w, b) = db.available_pairs[0]
    size = db.size(w, b)
    idx = rng.integers(0, size, size=500)
    mover, opp = ranking.unindex_batch(w, b, idx)
    canon = symmetry.is_canonical_batch(mover, opp)
    mover, opp = mover[canon], opp[canon]

    checked = 0
    for i in range(len(mover)):
        m, o = int(mover[i]), int(opp[i])
        base_cls, base_depth = db.lookup(m, o)
        k = int(rng.integers(0, symmetry.N_SYMS))
        m2, o2 = symmetry.apply(k, m), symmetry.apply(k, o)
        cls2, depth2 = db.lookup(m2, o2)
        assert cls2 == base_cls
        assert depth2 == base_depth
        checked += 1
    assert checked > 0


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_batch_shapes_and_dtypes():
    ds = NmmIterableDataset(DB_DIR, split="train", batch_size=256, seed=1)
    it = iter(ds)
    feats, labels, depths, depth_mask = next(it)
    assert feats.shape == (256, FEATURE_DIM)
    assert feats.dtype == torch.float32
    assert labels.shape == (256,)
    assert labels.dtype == torch.int64
    assert set(labels.tolist()) <= {LABEL_LOSS, LABEL_DRAW, LABEL_WIN}
    assert depths.shape == (256,)
    assert depth_mask.dtype == torch.bool
    # feature bits are exactly 0/1, counts fields in [0,1]
    assert ((feats[:, :48] == 0) | (feats[:, :48] == 1)).all()
    assert (feats[:, 48] >= 0).all() and (feats[:, 48] <= 1).all()


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_train_val_test_splits_disjoint_and_deterministic():
    from nmm.features import assign_split

    w, b = 3, 4
    idx = np.arange(0, 20000)
    s1 = assign_split(w, b, idx)
    s2 = assign_split(w, b, idx)
    assert (s1 == s2).all()  # deterministic
    # roughly the requested proportions (loose tolerance, small sample)
    val_frac = (s1 == 1).mean()
    test_frac = (s1 == 2).mean()
    assert 0.001 < val_frac < 0.02
    assert 0.001 < test_frac < 0.02


@pytest.mark.slow
@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_empirical_class_frequency_matches_full_scan_tally():
    """Sampled (non-augmented) class frequencies should match the exact
    full-scan tallies (ml/cache/db-stats-partial.txt) within a few percent."""
    baseline_path = (
        Path(__file__).resolve().parent.parent / "cache" / "db-stats-partial.txt"
    )
    if not baseline_path.exists():
        pytest.skip("baseline not captured")
    baseline = {}
    for line in baseline_path.read_text().splitlines():
        if not line.strip():
            continue
        head, rest = line.split(":")
        w, b = (int(x) for x in head.split("-"))
        fields = dict(kv.split("=") for kv in rest.split())
        baseline[(w, b)] = {k: int(v) for k, v in fields.items()}

    db = Database(DB_DIR)
    ds = NmmIterableDataset(DB_DIR, split="train", batch_size=4096, augment=False, seed=3)
    it = iter(ds)

    # Accumulate per-subspace class counts by re-deriving (w,b) from popcount
    # of the raw feature bits (labels alone don't carry subspace identity).
    from collections import defaultdict

    counts = defaultdict(lambda: {"wins": 0, "losses": 0, "draws": 0})
    n_total = 0
    target = 300_000
    while n_total < target:
        feats, labels, _, _ = next(it)
        mover_bits = feats[:, :24].numpy().astype(np.uint32)
        opp_bits = feats[:, 24:48].numpy().astype(np.uint32)
        w_counts = mover_bits.sum(axis=1).astype(int)
        b_counts = opp_bits.sum(axis=1).astype(int)
        lbl = labels.numpy()
        for w, b, label in zip(w_counts.tolist(), b_counts.tolist(), lbl.tolist()):
            if (w, b) not in baseline:
                continue
            if label == LABEL_WIN:
                counts[(w, b)]["wins"] += 1
            elif label == LABEL_LOSS:
                counts[(w, b)]["losses"] += 1
            else:
                counts[(w, b)]["draws"] += 1
        n_total += feats.shape[0]

    for (w, b), got in counts.items():
        exp = baseline[(w, b)]
        exp_total = exp["wins"] + exp["losses"] + exp["draws"]
        got_total = got["wins"] + got["losses"] + got["draws"]
        if got_total < 200:
            continue  # too few samples of a rare subspace to compare meaningfully
        for key in ("wins", "losses", "draws"):
            exp_frac = exp[key] / exp_total
            got_frac = got[key] / got_total
            assert abs(exp_frac - got_frac) < 0.03, (w, b, key, exp_frac, got_frac)


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_throughput_guideline():
    ds = NmmIterableDataset(DB_DIR, split="train", batch_size=8192, seed=4)
    it = iter(ds)
    next(it)  # warm up (lazy DB open, canonical-set cache load)
    start = time.time()
    n = 0
    for _ in range(5):
        feats, *_ = next(it)
        n += feats.shape[0]
    elapsed = time.time() - start
    rate = n / elapsed
    # Guideline from implementation-nn.md N4: if far below this, vectorize
    # harder before touching the trainer. Not a hard correctness gate.
    assert rate > 5_000, f"throughput {rate:.0f} samples/s is very low"
