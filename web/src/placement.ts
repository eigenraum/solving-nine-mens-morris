/**
 * Placement-phase (opening, plies 1-18) move generation and a trivial
 * heuristic move chooser. design-nn.md §10 / implementation-nn.md N8 step 5:
 * the opening is NOT model-backed in v1 -- this heuristic (prefer the point
 * that maximizes the mover's own mill-line potential minus the opponent's)
 * stands in until a placement-phase model is trained (N9, staged).
 */

import { FULL_MASK, MILLS } from "./board.js";
import { resolveMill } from "./rules.js";

export interface PlacementMove {
  to: number;
  captured: number | null;
  successorMover: number;
  successorOpp: number;
}

export function movesPlacement(mover: number, opponent: number): PlacementMove[] {
  const empty = (~(mover | opponent) & FULL_MASK) >>> 0;
  const moves: PlacementMove[] = [];
  let e = empty;
  while (e !== 0) {
    const to = 31 - Math.clz32(e & -e);
    e &= e - 1;
    const newMover = (mover | (1 << to)) >>> 0;
    for (const newOpp of resolveMill(newMover, opponent, to)) {
      let captured: number | null = null;
      if (newOpp !== opponent) {
        captured = 31 - Math.clz32((opponent & ~newOpp) >>> 0);
      }
      moves.push({ to, captured, successorMover: newOpp, successorOpp: newMover });
    }
  }
  return moves;
}

/** For each empty point, how many mills through it already have exactly one
 * of `side`'s stones and no stones of the other side (a rough "mill
 * potential" heuristic) -- not a value estimate, just an opening book
 * stand-in until N9 trains a placement-phase model. */
function millPotential(side: number, other: number, point: number): number {
  let score = 0;
  for (const mill of MILLS) {
    if (!(mill & (1 << point))) continue;
    const sideCount = popcountMask(mill & side);
    const otherCount = popcountMask(mill & other);
    if (otherCount === 0) score += sideCount;
  }
  return score;
}

function popcountMask(x: number): number {
  let c = 0;
  let m = x;
  while (m !== 0) {
    m &= m - 1;
    c++;
  }
  return c;
}

export function chooseOpeningMove(mover: number, opponent: number): PlacementMove | null {
  const moves = movesPlacement(mover, opponent);
  if (moves.length === 0) return null;

  let best = moves[0];
  let bestScore = -Infinity;
  for (const mv of moves) {
    let score = millPotential(mover, opponent, mv.to) - millPotential(opponent, mover, mv.to);
    if (mv.captured !== null) score += 100; // capturing is always at least as good
    if (score > bestScore) {
      bestScore = score;
      best = mv;
    }
  }
  return best;
}
