/**
 * Local "analysis provider": the in-browser neural engine exposed through the
 * exact same wire shapes as `ninemm serve`'s POST /api/analyze (src/server.rs),
 * so the unified UI (ui/index.html) can drive either engine through one code
 * path. Values additionally carry the network's raw WDL probabilities in an
 * optional `wdl` field ([loss, draw, win], from the perspective of the player
 * the value belongs to) that the exact backend never sets.
 *
 * Same perspective discipline as server.rs: the wire format uses physical
 * colors, the engine uses mover/opponent bitboards, and the conversion happens
 * in exactly two places (`toInternal` / `toStateJson`).
 */

import { popcount } from "./board.js";
import { pointName } from "./coords.js";
import { movesMovement, type Move } from "./rules.js";
import { movesPlacement, chooseOpeningMove, type PlacementMove } from "./placement.js";
import { chooseMove, Evaluator, loadModel, type NmmNetJS } from "./engine.js";

// ---------------------------------------------------------------------------
// Wire types (mirror src/server.rs)
// ---------------------------------------------------------------------------

export type Color = "white" | "black";
export type Outcome = "win" | "draw" | "loss";

export interface StateJson {
  white: number[];
  black: number[];
  turn: Color;
  whiteHand: number;
  blackHand: number;
}

export interface ValueJson {
  outcome: Outcome;
  plies: number | null;
  /** NN-only: [P(loss), P(draw), P(win)] for the value's owner. */
  wdl?: [number, number, number];
}

export interface MoveJson {
  from: number | null;
  to: number;
  capture: number | null;
  notation: string;
  value?: ValueJson;
  result: StateJson;
}

export interface ResultJson {
  winner: Color;
  reason: "fewerThanThree" | "noMoves";
}

export interface AnalyzeResponse {
  phase: "placement" | "movement";
  result: ResultJson | null;
  value?: ValueJson;
  moves: MoveJson[];
  engineMove: number | null;
}

export class AnalyzeError extends Error {}

// ---------------------------------------------------------------------------
// Conversions (the only two perspective-touching functions)
// ---------------------------------------------------------------------------

function pointsOf(mask: number): number[] {
  const out: number[] = [];
  for (let p = 0; p < 24; p++) if ((mask >>> p) & 1) out.push(p);
  return out;
}

/** Physical colors -> internal {mover, opp} bitboards. */
function toInternal(whiteBits: number, blackBits: number, turn: Color): { mover: number; opp: number } {
  return turn === "white"
    ? { mover: whiteBits, opp: blackBits }
    : { mover: blackBits, opp: whiteBits };
}

/** Internal {mover, opp} whose mover is the player `turn` -> wire state. */
function toStateJson(mover: number, opp: number, turn: Color, whiteHand: number, blackHand: number): StateJson {
  const [whiteBits, blackBits] = turn === "white" ? [mover, opp] : [opp, mover];
  return { white: pointsOf(whiteBits), black: pointsOf(blackBits), turn, whiteHand, blackHand };
}

// ---------------------------------------------------------------------------
// Validation (mirrors server.rs::validate)
// ---------------------------------------------------------------------------

function validate(state: StateJson): { whiteBits: number; blackBits: number } {
  const seen = { white: 0, black: 0 };
  for (const color of ["white", "black"] as const) {
    for (const p of state[color]) {
      if (!Number.isInteger(p) || p < 0 || p >= 24) {
        throw new AnalyzeError(`point index ${p} out of range 0..24`);
      }
      if ((seen[color] >>> p) & 1) throw new AnalyzeError(`duplicate ${color} point ${p}`);
      seen[color] |= 1 << p;
    }
  }
  if ((seen.white & seen.black) !== 0) {
    throw new AnalyzeError("white and black occupy the same point");
  }
  if (state.whiteHand > 9 || state.blackHand > 9) throw new AnalyzeError("hand count exceeds 9");
  if (state.white.length + state.whiteHand > 9) {
    throw new AnalyzeError("white has more than 9 total stones (board + hand)");
  }
  if (state.black.length + state.blackHand > 9) {
    throw new AnalyzeError("black has more than 9 total stones (board + hand)");
  }
  if (state.whiteHand > 0 || state.blackHand > 0) {
    const ok =
      state.turn === "white"
        ? state.whiteHand === state.blackHand
        : state.blackHand === state.whiteHand + 1;
    if (!ok) throw new AnalyzeError("hand counts are inconsistent with strict placement alternation");
  }
  return { whiteBits: seen.white >>> 0, blackBits: seen.black >>> 0 };
}

// ---------------------------------------------------------------------------
// Notation (mirrors server.rs::movement_notation / placement_notation)
// ---------------------------------------------------------------------------

function movementNotation(from: number, to: number, capture: number | null): string {
  let s = `${pointName(from)}-${pointName(to)}`;
  if (capture !== null) s += `x${pointName(capture)}`;
  return s;
}

function placementNotation(to: number, capture: number | null): string {
  let s = pointName(to);
  if (capture !== null) s += `x${pointName(capture)}`;
  return s;
}

// ---------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------

export interface NeuralEngineOptions {
  searchDepth: number;
  rootTTA: boolean;
}

export interface NeuralMeta {
  backend: "neural";
  hidden: number;
  nBlocks: number;
  /** The opening is heuristic, not model-backed (design-nn.md §10). */
  placementModelBacked: false;
}

export class NeuralEngine {
  private readonly net: NmmNetJS;
  private readonly evaluator: Evaluator;
  readonly options: NeuralEngineOptions;

  constructor(net: NmmNetJS, options: Partial<NeuralEngineOptions> = {}) {
    this.net = net;
    this.evaluator = new Evaluator(net);
    this.options = { searchDepth: 2, rootTTA: true, ...options };
  }

  meta(): NeuralMeta {
    return {
      backend: "neural",
      hidden: this.net.hidden,
      nBlocks: this.net.nBlocks,
      placementModelBacked: false,
    };
  }

  /** Same contract as POST /api/analyze. Throws AnalyzeError on bad input. */
  analyze(state: StateJson, evaluate: boolean): AnalyzeResponse {
    const { whiteBits, blackBits } = validate(state);
    const isPlacement = state.whiteHand > 0 || state.blackHand > 0;
    return isPlacement
      ? this.analyzePlacement(state, whiteBits, blackBits, evaluate)
      : this.analyzeMovement(state, whiteBits, blackBits, evaluate);
  }

  /** Value of the position for the side to move (a direct evaluation). */
  private positionValue(mover: number, opp: number): ValueJson {
    const e = this.evaluator.evaluateTTA(mover, opp);
    return valueFromWdl(e.wdlProbs, e.depth);
  }

  /** Value of having made a move, for the player who made it: the successor's
   * evaluation belongs to the *next* mover, so flip the WDL and add a ply
   * (same rule as server.rs::movement_move_value). */
  private moveValue(succMover: number, succOpp: number): ValueJson {
    if (popcount(succMover) < 3) {
      return { outcome: "win", plies: 1, wdl: [0, 0, 1] };
    }
    const e = this.evaluator.evaluateTTA(succMover, succOpp);
    const flipped: [number, number, number] = [e.wdlProbs[2], e.wdlProbs[1], e.wdlProbs[0]];
    return valueFromWdl(flipped, e.depth + 1);
  }

  private analyzeMovement(
    state: StateJson,
    whiteBits: number,
    blackBits: number,
    evaluate: boolean
  ): AnalyzeResponse {
    const turn = state.turn;
    const next: Color = turn === "white" ? "black" : "white";
    const { mover, opp } = toInternal(whiteBits, blackBits, turn);

    if (popcount(mover) < 3) return gameOver("movement", next, "fewerThanThree");
    if (popcount(opp) < 3) return gameOver("movement", turn, "fewerThanThree");
    const legal = movesMovement(mover, opp);
    if (legal.length === 0) return gameOver("movement", next, "noMoves");

    const moves: MoveJson[] = legal.map((m: Move) => ({
      from: m.src,
      to: m.dst,
      capture: m.captured,
      notation: movementNotation(m.src, m.dst, m.captured),
      ...(evaluate ? { value: this.moveValue(m.successorMover, m.successorOpp) } : {}),
      result: toStateJson(m.successorMover, m.successorOpp, next, 0, 0),
    }));

    const chosen = chooseMove(this.net, mover, opp, this.options);
    const engineMove = chosen
      ? legal.findIndex(
          (m) => m.successorMover === chosen.successorMover && m.successorOpp === chosen.successorOpp
        )
      : null;

    return {
      phase: "movement",
      result: null,
      ...(evaluate ? { value: this.positionValue(mover, opp) } : {}),
      moves,
      engineMove,
    };
  }

  private analyzePlacement(
    state: StateJson,
    whiteBits: number,
    blackBits: number,
    _evaluate: boolean
  ): AnalyzeResponse {
    const turn = state.turn;
    const next: Color = turn === "white" ? "black" : "white";
    const { mover, opp } = toInternal(whiteBits, blackBits, turn);
    const [moverHand, oppHand] =
      turn === "white" ? [state.whiteHand, state.blackHand] : [state.blackHand, state.whiteHand];

    // Total (board + hand) below three is a rule loss, as in server.rs.
    if (popcount(mover) + moverHand < 3) return gameOver("placement", next, "fewerThanThree");
    if (popcount(opp) + oppHand < 3) return gameOver("placement", turn, "fewerThanThree");

    const legal = movesPlacement(mover, opp);
    if (legal.length === 0) return gameOver("placement", next, "noMoves");

    // No values in the opening: the network is movement-phase only
    // (design-nn.md §10); the UI shows a notice instead of an overlay.
    const [nextWhiteHand, nextBlackHand] =
      turn === "white" ? [state.whiteHand - 1, state.blackHand] : [state.whiteHand, state.blackHand - 1];
    const moves: MoveJson[] = legal.map((m: PlacementMove) => ({
      from: null,
      to: m.to,
      capture: m.captured,
      notation: placementNotation(m.to, m.captured),
      result: toStateJson(m.successorMover, m.successorOpp, next, nextWhiteHand, nextBlackHand),
    }));

    const chosen = chooseOpeningMove(mover, opp);
    const engineMove = chosen
      ? legal.findIndex(
          (m) => m.successorMover === chosen.successorMover && m.successorOpp === chosen.successorOpp
        )
      : null;

    return { phase: "placement", result: null, moves, engineMove };
  }
}

function valueFromWdl(wdl: [number, number, number], depth: number): ValueJson {
  const [l, d, w] = wdl;
  const outcome: Outcome = w >= l && w >= d ? "win" : l >= d ? "loss" : "draw";
  // The depth head only means something for decided positions.
  const plies = outcome === "draw" ? null : Math.max(1, Math.round(depth));
  return { outcome, plies, wdl };
}

function gameOver(
  phase: "placement" | "movement",
  winner: Color,
  reason: ResultJson["reason"]
): AnalyzeResponse {
  return { phase, result: { winner, reason }, moves: [], engineMove: null };
}

/** Load the exported model from `baseUrl` (model.json + model.bin) and wrap it
 * as an analysis provider. Rejects if the export is missing. */
export async function createNeuralEngine(
  baseUrl: string,
  options: Partial<NeuralEngineOptions> = {}
): Promise<NeuralEngine> {
  const net = await loadModel(baseUrl);
  return new NeuralEngine(net, options);
}
