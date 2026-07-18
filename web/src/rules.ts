/**
 * Movement-phase move generation: slide/jump, mill closure, capture, terminals.
 *
 * Port of ml/nmm/movegen.py / src/movegen.rs / readme-database.md §6. A
 * position is a plain `{mover, opp}` pair of 24-bit numbers -- always "side
 * to move" normalized, matching the database convention. Every successor is
 * already perspective-flipped: its `mover` is the old opponent (minus the
 * captured stone, if any) and its `opp` is the old mover's updated stones.
 */

import { ADJ, FULL_MASK, POINT_MILLS, isMillAt, popcount } from "./board.js";

export interface Move {
  src: number;
  dst: number;
  captured: number | null;
  successorMover: number;
  successorOpp: number;
}

export function removableStones(opponent: number): number {
  let notInMill = 0;
  let m = opponent;
  while (m !== 0) {
    const p = 31 - Math.clz32(m & -m);
    m &= m - 1;
    if (!isMillAt(opponent, p)) notInMill |= 1 << p;
  }
  return notInMill !== 0 ? notInMill >>> 0 : opponent;
}

export function resolveMill(mover: number, opponent: number, dest: number): number[] {
  if (!isMillAt(mover, dest)) return [opponent];
  const removable = removableStones(opponent);
  const out: number[] = [];
  let m = removable;
  while (m !== 0) {
    const s = 31 - Math.clz32(m & -m);
    m &= m - 1;
    out.push((opponent & ~(1 << s)) >>> 0);
  }
  return out;
}

export function movesMovement(mover: number, opponent: number): Move[] {
  const empty = (~(mover | opponent) & FULL_MASK) >>> 0;
  const jump = popcount(mover) === 3;
  const moves: Move[] = [];
  let m = mover;
  while (m !== 0) {
    const src = 31 - Math.clz32(m & -m);
    m &= m - 1;
    const dests = jump ? empty : (ADJ[src] & empty) >>> 0;
    let d = dests;
    while (d !== 0) {
      const dst = 31 - Math.clz32(d & -d);
      d &= d - 1;
      const newMover = ((mover & ~(1 << src)) | (1 << dst)) >>> 0;
      for (const newOpp of resolveMill(newMover, opponent, dst)) {
        let captured: number | null = null;
        if (newOpp !== opponent) {
          captured = 31 - Math.clz32((opponent & ~newOpp) >>> 0);
        }
        moves.push({ src, dst, captured, successorMover: newOpp, successorOpp: newMover });
      }
    }
  }
  return moves;
}

export function isBlocked(mover: number, opponent: number): boolean {
  return movesMovement(mover, opponent).length === 0;
}

export const enum WdlClass {
  Loss = -1,
  Draw = 0,
  Win = 1,
}

/** WdlClass if `(mover, opponent)` is an immediate terminal for the side to
 * move, else null. Two terminal rules (readme-database.md §6): fewer than 3
 * stones, or no legal move. */
export function terminalClass(mover: number, opponent: number): WdlClass | null {
  if (popcount(mover) < 3) return WdlClass.Loss;
  if (isBlocked(mover, opponent)) return WdlClass.Loss;
  return null;
}
