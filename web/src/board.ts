/**
 * Board geometry: point numbering, adjacency, mills.
 *
 * Port of ml/nmm/board.py / src/board.rs / readme-database.md §1. 24 points on
 * three concentric rings (0=outer, 1=middle, 2=inner), 8 per ring. Point
 * `p = ring*8 + i`; within a ring, `i` runs clockwise from a fixed corner. Even
 * `i` are corners (degree 2), odd `i` are edge midpoints that also carry the
 * "spoke" edges connecting rings at the same `i`.
 *
 * Positions are represented as plain 24-bit numbers (bit p set = stone on
 * point p) -- fits safely in a JS number (< 2^53), no BigInt needed.
 */

export const N = 24;
export const N_MILLS = 16;
export const FULL_MASK = (1 << N) - 1 >>> 0; // 0x00FFFFFF, unsigned

function point(ring: number, i: number): number {
  return ring * 8 + (((i % 8) + 8) % 8);
}

function buildAdj(): number[] {
  const adj = new Array<number>(N).fill(0);
  for (let ring = 0; ring < 3; ring++) {
    for (let i = 0; i < 8; i++) {
      const p = point(ring, i);
      adj[p] |= 1 << point(ring, (i + 1) % 8);
      adj[p] |= 1 << point(ring, (i + 7) % 8);
    }
  }
  for (let i = 1; i < 8; i += 2) {
    const a = point(0, i);
    const b = point(1, i);
    const c = point(2, i);
    adj[a] |= 1 << b;
    adj[b] |= 1 << a;
    adj[b] |= 1 << c;
    adj[c] |= 1 << b;
  }
  return adj.map((x) => x >>> 0);
}

function buildMills(): number[] {
  const mills: number[] = [];
  for (let ring = 0; ring < 3; ring++) {
    for (let i = 0; i < 8; i += 2) {
      const a = point(ring, i);
      const b = point(ring, i + 1);
      const c = point(ring, i + 2);
      mills.push(((1 << a) | (1 << b) | (1 << c)) >>> 0);
    }
  }
  for (let i = 1; i < 8; i += 2) {
    const a = point(0, i);
    const b = point(1, i);
    const c = point(2, i);
    mills.push(((1 << a) | (1 << b) | (1 << c)) >>> 0);
  }
  if (mills.length !== N_MILLS) throw new Error("mill count mismatch");
  return mills;
}

function buildPointMills(mills: number[]): [number, number][] {
  const pm: number[][] = Array.from({ length: N }, () => []);
  for (const m of mills) {
    for (let p = 0; p < N; p++) {
      if (m & (1 << p)) pm[p].push(m);
    }
  }
  return pm.map((arr) => {
    if (arr.length !== 2) throw new Error("point not in exactly 2 mills");
    return [arr[0], arr[1]] as [number, number];
  });
}

export const ADJ: readonly number[] = buildAdj();
export const MILLS: readonly number[] = buildMills();
export const POINT_MILLS: readonly [number, number][] = buildPointMills(MILLS as number[]);

export function popcount(x: number): number {
  x = x - ((x >>> 1) & 0x55555555);
  x = (x & 0x33333333) + ((x >>> 2) & 0x33333333);
  x = (x + (x >>> 4)) & 0x0f0f0f0f;
  return (x * 0x01010101) >>> 24;
}

export function isMillAt(mask: number, p: number): boolean {
  const [m1, m2] = POINT_MILLS[p];
  return (mask & m1) === m1 || (mask & m2) === m2;
}
