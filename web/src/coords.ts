/**
 * Point <-> grid coordinate mapping for rendering, per readme-database.md §1's
 * standard notation: point `p = ring*8+i` maps to a 7x7 grid via half-width
 * `h = 3-ring`, then `(col,row)` from `i`. Display-only; never used for the
 * index scheme or move generation.
 */

export interface GridPos {
  col: number; // 1..7
  row: number; // 1..7
}

export function pointToGrid(p: number): GridPos {
  const ring = Math.floor(p / 8);
  const i = p % 8;
  const h = 3 - ring;
  switch (i) {
    case 0:
      return { col: 4 - h, row: 4 + h };
    case 1:
      return { col: 4, row: 4 + h };
    case 2:
      return { col: 4 + h, row: 4 + h };
    case 3:
      return { col: 4 + h, row: 4 };
    case 4:
      return { col: 4 + h, row: 4 - h };
    case 5:
      return { col: 4, row: 4 - h };
    case 6:
      return { col: 4 - h, row: 4 - h };
    case 7:
      return { col: 4 - h, row: 4 };
    default:
      throw new Error("unreachable");
  }
}

const COL_LETTERS = "abcdefg";

export function pointName(p: number): string {
  const { col, row } = pointToGrid(p);
  return `${COL_LETTERS[col - 1]}${row}`;
}
