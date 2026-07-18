/**
 * The 16-element board automorphism group and canonicalization.
 *
 * Port of ml/nmm/symmetry.py / src/symmetry.rs / readme-database.md §3. Point
 * `p = ring*8 + i` maps under symmetry `(a, b, s)` to `ring'*8 + i'` where
 * `i' = (a*i + b) mod 8` and `ring' = 2-ring if s else ring`, for
 * `a in {1, -1}`, `b in {0, 2, 4, 6}`, `s in {0, 1}` (16 combinations, generated
 * in that nested order to line up with the Python/Rust PERMS indices).
 */

import { N } from "./board.js";

export const N_SYMS = 16;

function symMap(a: number, b: number, s: number): number[] {
  const perm = new Array<number>(N);
  for (let p = 0; p < N; p++) {
    const ring = Math.floor(p / 8);
    const i = p % 8;
    const i2 = (((a * i + b) % 8) + 8) % 8;
    const ring2 = s ? 2 - ring : ring;
    perm[p] = ring2 * 8 + i2;
  }
  return perm;
}

function buildPerms(): number[][] {
  const perms: number[][] = [];
  for (const a of [1, -1]) {
    for (const b of [0, 2, 4, 6]) {
      for (const s of [0, 1]) {
        perms.push(symMap(a, b, s));
      }
    }
  }
  if (perms.length !== N_SYMS) throw new Error("symmetry count mismatch");
  return perms;
}

/** PERMS[k][p] = the point that p maps to under symmetry k. */
export const PERMS: readonly number[][] = buildPerms();

export function apply(k: number, mask: number): number {
  const perm = PERMS[k];
  let out = 0;
  let m = mask >>> 0;
  while (m !== 0) {
    const p = 31 - Math.clz32(m & -m);
    m &= m - 1;
    out |= 1 << perm[p];
  }
  return out >>> 0;
}

export function applyPos(k: number, mover: number, opp: number): [number, number] {
  return [apply(k, mover), apply(k, opp)];
}

/** Canonical form: minimum (mover, opp) lexicographically (mover primary)
 * over all 16 symmetry images -- matches readme-database.md §3 exactly. */
export function canonicalize(mover: number, opp: number): [number, number, number] {
  let bestM = apply(0, mover);
  let bestO = apply(0, opp);
  let bestSym = 0;
  for (let k = 1; k < N_SYMS; k++) {
    const m = apply(k, mover);
    const o = apply(k, opp);
    if (m < bestM || (m === bestM && o < bestO)) {
      bestM = m;
      bestO = o;
      bestSym = k;
    }
  }
  return [bestM, bestO, bestSym];
}

export function isCanonical(mover: number, opp: number): boolean {
  const [cm, co] = canonicalize(mover, opp);
  return cm === mover && co === opp;
}
