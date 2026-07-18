"""Reader for the `db/` directory produced by `ninemm solve`.

Independent Python port of the reading half of `src/persist.rs` /
`readme-database.md` §§5-7. Memmaps the raw little-endian u16 subspace files and
decodes values per the WDL/depth encoding. Shares no code with the Rust crate --
this file (plus `ranking.py` / `symmetry.py` / `board.py`) is the Python side of
the "independent oracle" this project's testing philosophy calls for
(`readme-agent.md`): if this reader agrees with `ninemm db-stats` / `ninemm
verify`, both implementations of the format spec are very likely correct.
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np

from . import ranking, symmetry
from .board import FULL_MASK

DRAW = 0xFFFF

# WDL class codes used throughout this package (not the on-disk encoding):
LOSS, DRAW_CLASS, WIN = -1, 0, 1


class Database:
    """Memory-mapped view of a `db/` directory.

    `available_pairs` lists the ordered `(w, b)` subspaces actually present
    (manifest entry + file of the declared size) -- a partial database (e.g.
    from `ninemm solve --max-total N`) is handled transparently; everything in
    this module only ever touches pairs the manifest actually lists.
    """

    def __init__(self, directory: str | Path):
        self.dir = Path(directory)
        manifest_path = self.dir / "manifest.json"
        with open(manifest_path) as f:
            manifest = json.load(f)

        self._entries: dict[tuple[int, int], dict] = {}
        self._arrays: dict[tuple[int, int], np.memmap] = {}
        for e in manifest["entries"]:
            w, b, size = e["w"], e["b"], e["size"]
            path = self.dir / f"wdl_{w}_{b}.bin"
            if not path.exists():
                continue
            arr = np.memmap(path, dtype="<u2", mode="r")
            if arr.shape[0] != size:
                raise ValueError(
                    f"wdl_{w}_{b}.bin has {arr.shape[0]} entries, "
                    f"manifest declares {size}"
                )
            self._entries[(w, b)] = e
            self._arrays[(w, b)] = arr

    @property
    def available_pairs(self) -> list[tuple[int, int]]:
        return sorted(self._entries.keys())

    def has(self, w: int, b: int) -> bool:
        return (w, b) in self._arrays

    def size(self, w: int, b: int) -> int:
        return self._entries[(w, b)]["size"]

    def raw(self, w: int, b: int, idx) -> np.ndarray:
        """Raw stored u16 value(s) at index/indices `idx` in subspace (w, b)."""
        return np.asarray(self._arrays[(w, b)])[np.asarray(idx)]

    # -- decoding ------------------------------------------------------

    @staticmethod
    def decode(raw_value: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
        """Decode raw u16 code(s) into (wdl_class, depth).

        wdl_class in {-1 loss, 0 draw, +1 win} for the side to move; depth is
        the stored ply count (0 for draws, by convention -- see
        `readme-database.md` §5: even=loss, odd=win).
        """
        raw_value = np.asarray(raw_value)
        is_draw = raw_value == DRAW
        depth = np.where(is_draw, 0, raw_value).astype(np.int64)
        is_win = (~is_draw) & (raw_value % 2 == 1)
        is_loss = (~is_draw) & (raw_value % 2 == 0)
        cls = np.where(is_draw, DRAW_CLASS, np.where(is_win, WIN, LOSS))
        return cls.astype(np.int8), depth

    # -- lookup ----------------------------------------------------------

    def lookup_raw(self, mover: int, opp: int) -> int:
        """Raw u16 code for a position, canonicalizing and indexing first.

        The caller must ensure `opp` has >= 3 stones (opponent below 3 stones
        is an implicit Loss(0), not represented in any file -- see
        `readme-database.md` §5 and `movegen.py`'s terminal handling).
        """
        w, b, idx = ranking.index(mover, opp)
        if not self.has(w, b):
            raise KeyError(f"subspace ({w},{b}) not present in this database")
        return int(self.raw(w, b, idx))

    def lookup(self, mover: int, opp: int) -> tuple[int, int]:
        """(wdl_class, depth) for a position, for the side to move."""
        raw_value = self.lookup_raw(mover, opp)
        cls, depth = self.decode(np.array([raw_value]))
        return int(cls[0]), int(depth[0])

    def tally(self, w: int, b: int) -> dict:
        """Win/loss/draw counts and max depths over *canonical* slots only
        (mirrors `ninemm db-stats`, which the N2 gate diffs against).
        """
        arr = self._arrays[(w, b)]
        n = arr.shape[0]
        wins = losses = draws = 0
        max_win_depth = max_loss_depth = 0
        chunk = 1 << 20
        for start in range(0, n, chunk):
            end = min(start + chunk, n)
            idx = np.arange(start, end, dtype=np.int64)
            mover, opp = ranking.unindex_batch(w, b, idx)
            canon = symmetry.is_canonical_batch(mover, opp)
            values = np.asarray(arr[start:end])[canon]
            cls, depth = self.decode(values)
            wins += int((cls == WIN).sum())
            losses += int((cls == LOSS).sum())
            draws += int((cls == DRAW_CLASS).sum())
            if (cls == WIN).any():
                max_win_depth = max(max_win_depth, int(depth[cls == WIN].max()))
            if (cls == LOSS).any():
                max_loss_depth = max(max_loss_depth, int(depth[cls == LOSS].max()))
        return {
            "wins": wins,
            "losses": losses,
            "draws": draws,
            "max_win_depth": max_win_depth,
            "max_loss_depth": max_loss_depth,
        }
