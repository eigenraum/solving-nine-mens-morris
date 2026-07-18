"""Streaming training data: samples real (canonical, non-wasted) database slots
i.i.d., weighted by subspace size, with symmetry augmentation and a
deterministic train/val/test split -- design-nn.md §6.
"""

from __future__ import annotations

from pathlib import Path

import numpy as np
import torch
from torch.utils.data import IterableDataset

from . import ranking, symmetry
from .db import DRAW_CLASS, Database
from .features import TEST, TRAIN, VAL, assign_split, featurize

# WDL label indices for the 3-way softmax head (design-nn.md §2). Distinct
# from db.py's {-1,0,1} WDL-class convention (kept separate there to match the
# on-disk parity rule directly); this is the CrossEntropyLoss target space.
LABEL_LOSS, LABEL_DRAW, LABEL_WIN = 0, 1, 2


def class_to_label(cls: np.ndarray) -> np.ndarray:
    """db.py class {-1,0,1} -> training label {0,1,2}."""
    return (cls + 1).astype(np.int64)


class NmmIterableDataset(IterableDataset):
    """Infinite stream of pre-batched (features, labels, depth_targets,
    depth_mask) tuples. Use with `DataLoader(ds, batch_size=None,
    num_workers=k)` -- each yielded item is already a full batch, so
    DataLoader's own batching/collation must be disabled.
    """

    def __init__(
        self,
        db_dir: str | Path,
        split: str = "train",
        batch_size: int = 8192,
        augment: bool | None = None,
        seed: int = 0,
        val_frac: float = 0.005,
        test_frac: float = 0.005,
        oversample: float = 4.0,
        only_pairs: list[tuple[int, int]] | None = None,
    ):
        assert split in ("train", "val", "test")
        self.db_dir = str(db_dir)
        self.split = split
        self.batch_size = batch_size
        self.augment = augment if augment is not None else (split == "train")
        self.seed = seed
        self.val_frac = val_frac
        self.test_frac = test_frac
        self.oversample = oversample
        self.only_pairs = set(only_pairs) if only_pairs else None

    def _split_code(self) -> int:
        return {"train": TRAIN, "val": VAL, "test": TEST}[self.split]

    def __iter__(self):
        worker_info = torch.utils.data.get_worker_info()
        worker_id = worker_info.id if worker_info else 0
        rng = np.random.default_rng((self.seed, worker_id, hash(self.split) & 0xFFFF))
        db = Database(self.db_dir)
        pairs = db.available_pairs
        if self.only_pairs is not None:
            pairs = [p for p in pairs if p in self.only_pairs]
            if not pairs:
                raise ValueError(f"none of only_pairs={self.only_pairs} are in the database")
        sizes = np.array([db.size(w, b) for (w, b) in pairs], dtype=np.float64)
        probs = sizes / sizes.sum()
        split_code = self._split_code()

        while True:
            batch = self._draw_batch(db, pairs, probs, rng, split_code)
            if batch is None:
                continue
            feats, labels, depths, depth_mask = batch
            yield (
                torch.from_numpy(feats),
                torch.from_numpy(labels),
                torch.from_numpy(depths),
                torch.from_numpy(depth_mask),
            )

    def _draw_batch(self, db, pairs, probs, rng, split_code):
        draw_n = int(self.batch_size * self.oversample) + 1
        pair_choice = rng.choice(len(pairs), size=draw_n, p=probs)

        movers = np.zeros(draw_n, dtype=np.uint32)
        opps = np.zeros(draw_n, dtype=np.uint32)
        classes = np.zeros(draw_n, dtype=np.int8)
        depths_raw = np.zeros(draw_n, dtype=np.int64)
        keep = np.zeros(draw_n, dtype=bool)

        for gi, (w, b) in enumerate(pairs):
            sel = pair_choice == gi
            cnt = int(sel.sum())
            if cnt == 0:
                continue
            size = db.size(w, b)
            idx = rng.integers(0, size, size=cnt)
            m, o = ranking.unindex_batch(w, b, idx)
            canon = symmetry.is_canonical_batch(m, o)
            sp = assign_split(w, b, idx, self.val_frac, self.test_frac)
            use = canon & (sp == split_code)

            raw = db.raw(w, b, idx)
            cls, dep = db.decode(raw)

            movers[sel] = m
            opps[sel] = o
            classes[sel] = cls
            depths_raw[sel] = dep
            keep[sel] = use

        movers, opps = movers[keep], opps[keep]
        classes, depths_raw = classes[keep], depths_raw[keep]
        n = movers.shape[0]
        if n == 0:
            return None

        if self.augment:
            ks = rng.integers(0, symmetry.N_SYMS, size=n)
            m2, o2 = movers.copy(), opps.copy()
            for k in range(symmetry.N_SYMS):
                ksel = ks == k
                if not ksel.any():
                    continue
                m2[ksel] = symmetry.apply_batch(k, movers[ksel])
                o2[ksel] = symmetry.apply_batch(k, opps[ksel])
            movers, opps = m2, o2

        n_out = min(n, self.batch_size)
        feats = featurize(movers[:n_out], opps[:n_out])
        labels = class_to_label(classes[:n_out])
        depths = (depths_raw[:n_out].astype(np.float32)) / 255.0
        depth_mask = classes[:n_out] != DRAW_CLASS
        return feats, labels, depths, depth_mask
