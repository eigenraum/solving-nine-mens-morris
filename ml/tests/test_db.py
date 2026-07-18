from pathlib import Path

import numpy as np
import pytest

from nmm.db import DRAW, LOSS, DRAW_CLASS, WIN, Database

DB_DIR = Path(__file__).resolve().parent.parent.parent / "db"
HAVE_DB = (DB_DIR / "manifest.json").exists()


def test_decode_draw():
    cls, depth = Database.decode(np.array([DRAW], dtype=np.uint16))
    assert cls[0] == DRAW_CLASS
    assert depth[0] == 0


def test_decode_even_is_loss_odd_is_win():
    cls, depth = Database.decode(np.array([0, 1, 2, 3, 26, 31], dtype=np.uint16))
    assert list(cls) == [LOSS, WIN, LOSS, WIN, LOSS, WIN]
    assert list(depth) == [0, 1, 2, 3, 26, 31]


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_manifest_loads_and_pairs_present():
    d = Database(DB_DIR)
    assert (3, 3) in d.available_pairs
    assert d.size(3, 3) == 210140


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_gasser_fig9_longest_loss_in_3_3_is_26_plies():
    """Cross-check against the same published number design.md/README cite:
    a {3,3} position lost in exactly 26 plies (Gasser Fig. 9)."""
    d = Database(DB_DIR)
    arr = np.asarray(d._arrays[(3, 3)])
    cls, depth = Database.decode(arr)
    assert depth[cls == LOSS].max() == 26


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_gasser_fig11_3_3_win_rate_near_83_percent():
    d = Database(DB_DIR)
    t = d.tally(3, 3)
    total = t["wins"] + t["losses"] + t["draws"]
    win_rate = t["wins"] / total
    assert 0.80 < win_rate < 0.86


@pytest.mark.skipif(not HAVE_DB, reason="db/ not present")
def test_lookup_matches_raw_array_at_a_known_slot():
    d = Database(DB_DIR)
    raw0 = int(np.asarray(d._arrays[(3, 3)])[0])
    from nmm import ranking

    mover, opp = ranking.unindex(3, 3, 0)
    assert d.lookup_raw(mover, opp) == raw0
