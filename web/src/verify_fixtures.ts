/**
 * Parity harness: replays fixtures exported by `ml/nmm/export.py` against
 * board.ts / symmetry.ts / rules.ts and diffs exactly. This is the N8 gate
 * from implementation-nn.md: geometry (adjacency, mills, symmetry
 * permutations) must match the Python/Rust tables bit-for-bit, and move
 * generation must reproduce >=10^4 (position, legal-move-set) traces exactly.
 *
 * Usage: node dist/verify_fixtures.js <fixtures-dir>
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { ADJ, MILLS, POINT_MILLS } from "./board.js";
import { PERMS } from "./symmetry.js";
import { movesMovement } from "./rules.js";

interface GeometryFixture {
  adj: number[];
  mills: number[];
  point_mills: [number, number][];
  perms: number[][];
}

interface TraceMove {
  src: number;
  dst: number;
  captured: number | null;
  successorMover: number;
  successorOpp: number;
}

interface Trace {
  mover: number;
  opp: number;
  moves: TraceMove[];
}

function checkGeometry(dir: string): number {
  const fixture: GeometryFixture = JSON.parse(
    readFileSync(join(dir, "geometry.json"), "utf-8")
  );
  let errors = 0;

  for (let p = 0; p < 24; p++) {
    if ((ADJ[p] >>> 0) !== (fixture.adj[p] >>> 0)) {
      console.error(`ADJ mismatch at point ${p}: got ${ADJ[p]} want ${fixture.adj[p]}`);
      errors++;
    }
  }
  if (MILLS.length !== fixture.mills.length) {
    console.error(`MILLS length mismatch: got ${MILLS.length} want ${fixture.mills.length}`);
    errors++;
  } else {
    const gotSet = new Set(MILLS.map((m) => m >>> 0));
    const wantSet = new Set(fixture.mills.map((m) => m >>> 0));
    if (gotSet.size !== wantSet.size || [...gotSet].some((m) => !wantSet.has(m))) {
      console.error("MILLS set mismatch");
      errors++;
    }
  }
  for (let p = 0; p < 24; p++) {
    const got = new Set(POINT_MILLS[p]);
    const want = new Set(fixture.point_mills[p]);
    if (got.size !== want.size || [...got].some((m) => !want.has(m))) {
      console.error(`POINT_MILLS mismatch at point ${p}`);
      errors++;
    }
  }
  if (PERMS.length !== fixture.perms.length) {
    console.error("PERMS length mismatch");
    errors++;
  } else {
    for (let k = 0; k < PERMS.length; k++) {
      for (let p = 0; p < 24; p++) {
        if (PERMS[k][p] !== fixture.perms[k][p]) {
          console.error(`PERMS mismatch at sym ${k} point ${p}`);
          errors++;
        }
      }
    }
  }

  console.log(`geometry: ${errors === 0 ? "OK" : `${errors} ERRORS`}`);
  return errors;
}

function moveKey(m: TraceMove): string {
  return `${m.src},${m.dst},${m.captured},${m.successorMover},${m.successorOpp}`;
}

function checkRulesTrace(dir: string): number {
  const traces: Trace[] = JSON.parse(readFileSync(join(dir, "rules_trace.json"), "utf-8"));
  let errors = 0;
  let checked = 0;

  for (const t of traces) {
    const got = movesMovement(t.mover, t.opp);
    if (got.length !== t.moves.length) {
      console.error(
        `move count mismatch at mover=${t.mover} opp=${t.opp}: got ${got.length} want ${t.moves.length}`
      );
      errors++;
      continue;
    }
    const gotKeys = new Set(
      got.map((m) =>
        moveKey({
          src: m.src,
          dst: m.dst,
          captured: m.captured,
          successorMover: m.successorMover,
          successorOpp: m.successorOpp,
        })
      )
    );
    const wantKeys = new Set(t.moves.map(moveKey));
    if (gotKeys.size !== wantKeys.size || [...gotKeys].some((k) => !wantKeys.has(k))) {
      console.error(`move set mismatch at mover=${t.mover} opp=${t.opp}`);
      errors++;
    }
    checked++;
  }

  console.log(`rules trace: ${checked} positions checked, ${errors === 0 ? "OK" : `${errors} ERRORS`}`);
  return errors;
}

function main() {
  const dir = process.argv[2] ?? "fixtures";
  const errors = checkGeometry(dir) + checkRulesTrace(dir);
  if (errors > 0) {
    console.error(`FAILED with ${errors} error(s)`);
    process.exit(1);
  }
  console.log("all fixture checks passed");
}

main();
