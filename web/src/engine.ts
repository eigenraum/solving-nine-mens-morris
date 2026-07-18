/**
 * The local opponent: alpha-beta search over movement-phase positions with
 * exact terminal rules at interior nodes and network evaluations at leaves,
 * plus root-level TTA (test-time augmentation: average over all 16 symmetry
 * images). design-nn.md §9.
 *
 * Move selection follows readme-database.md §5's rule read against
 * predicted values: prefer a successor classified LOSS (for its own mover)
 * with minimal depth, else DRAW, else WIN with maximal depth. A capture
 * dropping the opponent below 3 stones is an immediate win, handled by rule
 * -- never passed to the net.
 */

import { popcount } from "./board.js";
import { N_SYMS, apply } from "./symmetry.js";
import { movesMovement, terminalClass, type Move, WdlClass } from "./rules.js";
import { NmmNetJS, softmax3 } from "./nn.js";

export interface Evaluation {
  wdlProbs: [number, number, number]; // [loss, draw, win] for the side to move
  depth: number; // in plies, already de-normalized (* depth_scale)
}

const FEATURE_DIM = 52;

function featurize(mover: number, opp: number, out: Float32Array): void {
  for (let p = 0; p < 24; p++) {
    out[p] = (mover >>> p) & 1;
    out[24 + p] = (opp >>> p) & 1;
  }
  const wc = popcount(mover);
  const bc = popcount(opp);
  out[48] = wc / 9;
  out[49] = bc / 9;
  out[50] = wc === 3 ? 1 : 0;
  out[51] = bc === 3 ? 1 : 0;
}

export class Evaluator {
  private readonly net: NmmNetJS;
  private readonly depthScale: number;
  private readonly scratch = new Float32Array(FEATURE_DIM);

  constructor(net: NmmNetJS, depthScale = 255) {
    this.net = net;
    this.depthScale = depthScale;
  }

  /** Single forward pass, no augmentation -- used inside search nodes where
   * throughput matters more than the marginal accuracy TTA buys. */
  evaluate(mover: number, opp: number): Evaluation {
    featurize(mover, opp, this.scratch);
    const { wdlLogits, depth } = this.net.forward(this.scratch);
    return { wdlProbs: softmax3(wdlLogits), depth: depth * this.depthScale };
  }

  /** Averaged over all 16 symmetry images -- used at the root for the
   * player's actual move choice and for the training tool's displayed
   * evaluations, where the extra ~16x cost is negligible. */
  evaluateTTA(mover: number, opp: number): Evaluation {
    let l = 0,
      d = 0,
      w = 0,
      depthSum = 0;
    for (let k = 0; k < N_SYMS; k++) {
      const m = apply(k, mover);
      const o = apply(k, opp);
      const e = this.evaluate(m, o);
      l += e.wdlProbs[0];
      d += e.wdlProbs[1];
      w += e.wdlProbs[2];
      depthSum += e.depth;
    }
    return { wdlProbs: [l / N_SYMS, d / N_SYMS, w / N_SYMS], depth: depthSum / N_SYMS };
  }
}

interface ScoredMove {
  move: Move;
  tier: 0 | 1 | 2; // 0 = opponent loses (good for us), 1 = draw, 2 = opponent wins
  tiebreak: number; // for tier 0: successor's loss depth (prefer smaller); tier 2: successor's win depth (prefer larger, stored negated)
}

/** 1-ply move ranking using an Evaluation source (design-nn.md §2/§5). Moves
 * that immediately win by rule (opponent capped below 3 stones) always rank
 * first, before any network evaluation. */
function scoreMoves(
  moves: Move[],
  evalFn: (mover: number, opp: number) => Evaluation
): ScoredMove[] {
  return moves.map((move) => {
    if (popcount(move.successorMover) < 3) {
      return { move, tier: 0, tiebreak: -Infinity }; // immediate win, best possible
    }
    const e = evalFn(move.successorMover, move.successorOpp);
    const label = e.wdlProbs.indexOf(Math.max(...e.wdlProbs));
    if (label === 0) return { move, tier: 0, tiebreak: e.depth }; // successor LOSS
    if (label === 1) return { move, tier: 1, tiebreak: 0 };
    return { move, tier: 2, tiebreak: -e.depth }; // successor WIN: prefer larger depth
  });
}

function pickBest(scored: ScoredMove[]): Move {
  let best = scored[0];
  for (const s of scored) {
    if (s.tier < best.tier || (s.tier === best.tier && s.tiebreak < best.tiebreak)) {
      best = s;
    }
  }
  return best.move;
}

/** Minimax value used inside alpha-beta interior nodes: exact terminal rules
 * where they apply, network leaf evaluation otherwise. Returns a scalar in
 * [-1, 1] roughly (win prob - loss prob) from `mover`'s perspective, used
 * only to order/prune -- final move choice at the root still goes through
 * `scoreMoves`'s tiered rule for depth-aware play. */
function negamax(
  evaluator: Evaluator,
  mover: number,
  opp: number,
  depth: number,
  alpha: number,
  beta: number
): number {
  const term = terminalClass(mover, opp);
  if (term === WdlClass.Loss) return -1;

  if (depth === 0) {
    const e = evaluator.evaluate(mover, opp);
    return e.wdlProbs[2] - e.wdlProbs[0];
  }

  const moves = movesMovement(mover, opp);
  let value = -Infinity;
  for (const mv of moves) {
    let childValue: number;
    if (popcount(mv.successorMover) < 3) {
      childValue = 1; // opponent capped below 3 -- immediate win for us
    } else {
      childValue = -negamax(evaluator, mv.successorMover, mv.successorOpp, depth - 1, -beta, -alpha);
    }
    if (childValue > value) value = childValue;
    if (value > alpha) alpha = value;
    if (alpha >= beta) break;
  }
  return value;
}

export interface EngineOptions {
  searchDepth: number; // 0 = pure 1-ply net move selection, 2-4 = alpha-beta
  rootTTA: boolean;
}

const DEFAULT_OPTIONS: EngineOptions = { searchDepth: 2, rootTTA: true };

/** Chooses a move for `(mover, opp)`. Root move selection always uses the
 * tiered depth-aware rule (design-nn.md §5); `searchDepth` controls whether
 * each candidate successor is scored by a single network call (depth 0) or
 * by an alpha-beta search rooted at that successor (depth >= 2, evaluating
 * the successor's own best continuation instead of just its own leaf
 * value -- still combined with the same tiered rule via the successor's own
 * one-ply network read for the WDL *class*, since only the class needs to be
 * decided at the root and depth search over-refines the class prediction
 * where it matters most: draw/win boundary cases). */
export function chooseMove(
  net: NmmNetJS,
  mover: number,
  opp: number,
  options: Partial<EngineOptions> = {}
): Move | null {
  const opts = { ...DEFAULT_OPTIONS, ...options };
  const moves = movesMovement(mover, opp);
  if (moves.length === 0) return null;

  const evaluator = new Evaluator(net);
  const rootEvalFn = opts.rootTTA
    ? (m: number, o: number) => evaluator.evaluateTTA(m, o)
    : (m: number, o: number) => evaluator.evaluate(m, o);

  if (opts.searchDepth === 0) {
    return pickBest(scoreMoves(moves, rootEvalFn));
  }

  // Depth >= 2: refine each candidate's WDL class via a deeper negamax
  // search, but keep depth-aware tie-breaking from the shallow (root) read
  // -- the search value only reorders tier boundaries (draw vs win/loss),
  // it doesn't produce plies-to-mate on its own.
  const scored = moves.map((move) => {
    if (popcount(move.successorMover) < 3) {
      return { move, tier: 0 as const, tiebreak: -Infinity };
    }
    const shallow = rootEvalFn(move.successorMover, move.successorOpp);
    const searchValue = -negamax(
      evaluator,
      move.successorMover,
      move.successorOpp,
      opts.searchDepth - 1,
      -Infinity,
      Infinity
    );
    // searchValue in [-1,1] from our own perspective after this move;
    // combine with the shallow read's tier for depth tie-breaking.
    let tier: 0 | 1 | 2;
    if (searchValue > 0.34) tier = 0;
    else if (searchValue < -0.34) tier = 2;
    else tier = 1;
    const tiebreak = tier === 0 ? shallow.depth : tier === 2 ? -shallow.depth : 0;
    return { move, tier, tiebreak };
  });
  return pickBest(scored);
}

export { NmmNetJS } from "./nn.js";
export { loadModel } from "./nn.js";
