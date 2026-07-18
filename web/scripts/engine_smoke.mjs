/**
 * Exercises the real deployment code path (nn.ts's forward pass + engine.ts's
 * search/TTA/move-selection) against the actual exported model and real
 * movement-phase positions sampled from the database (via the rules_trace
 * fixture, so every position is guaranteed legal/canonical) -- no browser,
 * no DOM-click simulation, just the exact module graph the browser would run.
 *
 * Usage: node scripts/engine_smoke.mjs  (run after `npm run build`)
 */

import { readFileSync } from "node:fs";
import { NmmNetJS } from "../dist/nn.js";
import { chooseMove } from "../dist/engine.js";
import { movesMovement } from "../dist/rules.js";
import { popcount } from "../dist/board.js";

const manifest = JSON.parse(readFileSync("export/model.json", "utf-8"));
const blobBuf = readFileSync("export/model.bin");
const blob = blobBuf.buffer.slice(blobBuf.byteOffset, blobBuf.byteOffset + blobBuf.byteLength);
const net = new NmmNetJS(manifest, blob);

const traces = JSON.parse(readFileSync("fixtures/rules_trace.json", "utf-8"));

let checked = 0;
let illegalMoves = 0;
const timings = [];

for (const t of traces.slice(0, 15)) {
  if (popcount(t.mover) < 3) continue; // terminal, no move to choose
  const legal = movesMovement(t.mover, t.opp);
  if (legal.length === 0) continue; // blocked, terminal

  for (const depth of [0, 1]) {
    const start = performance.now();
    const mv = chooseMove(net, t.mover, t.opp, { searchDepth: depth, rootTTA: depth === 0 });
    const elapsed = performance.now() - start;
    if (depth === 1) timings.push(elapsed);

    if (!mv) {
      console.error(`chooseMove returned null for a position with ${legal.length} legal moves`);
      process.exit(1);
    }
    const isLegal = legal.some(
      (m) =>
        m.src === mv.src &&
        m.dst === mv.dst &&
        m.captured === mv.captured &&
        m.successorMover === mv.successorMover &&
        m.successorOpp === mv.successorOpp
    );
    if (!isLegal) {
      illegalMoves++;
      console.error(
        `chooseMove (depth=${depth}) returned an illegal move at mover=${t.mover} opp=${t.opp}`
      );
    }
  }
  checked++;
}

const avgMs = timings.reduce((a, b) => a + b, 0) / timings.length;
console.log(`checked ${checked} positions (depth 0 and depth 1 each)`);
console.log(`illegal moves returned: ${illegalMoves}`);
console.log(`avg depth-1 search time: ${avgMs.toFixed(2)}ms`);

if (illegalMoves > 0) {
  console.error("FAILED: chooseMove returned illegal move(s)");
  process.exit(1);
}
console.log(
  "engine smoke test passed: chooseMove always returns a legal move (depth 0 and depth 1)"
);
