/**
 * Browser demo: local play against the compressed engine, plus a training-
 * tool panel showing per-move WDL evaluations. See public/index.html.
 */

import { ADJ, N } from "./board.js";
import { pointToGrid } from "./coords.js";
import { movesMovement, terminalClass, type Move, WdlClass } from "./rules.js";
import { movesPlacement, chooseOpeningMove, type PlacementMove } from "./placement.js";
import { chooseMove, Evaluator, loadModel, type NmmNetJS } from "./engine.js";

const SVG_NS = "http://www.w3.org/2000/svg";
const MARGIN = 60;
const STEP = (480 - 2 * MARGIN) / 6;

type Side = "white" | "black";

interface GameState {
  white: number;
  black: number;
  whiteLeft: number;
  blackLeft: number;
  sideToMove: Side;
  humanSide: Side;
}

let state: GameState;
let net: NmmNetJS | null = null;
let evaluator: Evaluator | null = null;
let selectedSrc: number | null = null;
let awaitingCapture: { candidates: number[]; resolve: (p: number) => void } | null = null;

const statusEl = document.getElementById("status")!;
const evalListEl = document.getElementById("evalList")!;
const svg = document.getElementById("board") as unknown as SVGSVGElement;

function pixelOf(p: number): { x: number; y: number } {
  const { col, row } = pointToGrid(p);
  return { x: MARGIN + (col - 1) * STEP, y: MARGIN + (7 - row) * STEP };
}

function newGame(humanSide: Side): GameState {
  return {
    white: 0,
    black: 0,
    whiteLeft: 9,
    blackLeft: 9,
    sideToMove: "white",
    humanSide,
  };
}

function mover(s: GameState): number {
  return s.sideToMove === "white" ? s.white : s.black;
}
function opp(s: GameState): number {
  return s.sideToMove === "white" ? s.black : s.white;
}
function setMoverOpp(s: GameState, newMover: number, newOpp: number): void {
  if (s.sideToMove === "white") {
    s.white = newMover;
    s.black = newOpp;
  } else {
    s.black = newMover;
    s.white = newOpp;
  }
}
function otherSide(s: Side): Side {
  return s === "white" ? "black" : "white";
}
function isPlacementPhase(s: GameState): boolean {
  return s.whiteLeft > 0 || s.blackLeft > 0;
}

function render(): void {
  while (svg.firstChild) svg.removeChild(svg.firstChild);

  for (let p = 0; p < N; p++) {
    let m = ADJ[p];
    while (m !== 0) {
      const q = 31 - Math.clz32(m & -m);
      m &= m - 1;
      if (q < p) continue; // draw each edge once
      const a = pixelOf(p);
      const b = pixelOf(q);
      const line = document.createElementNS(SVG_NS, "line");
      line.setAttribute("x1", String(a.x));
      line.setAttribute("y1", String(a.y));
      line.setAttribute("x2", String(b.x));
      line.setAttribute("y2", String(b.y));
      line.setAttribute("class", "line");
      svg.appendChild(line);
    }
  }

  for (let p = 0; p < N; p++) {
    const { x, y } = pixelOf(p);
    const occupiedWhite = (state.white >>> p) & 1;
    const occupiedBlack = (state.black >>> p) & 1;

    const circle = document.createElementNS(SVG_NS, "circle");
    circle.setAttribute("cx", String(x));
    circle.setAttribute("cy", String(y));
    circle.setAttribute("r", "10");
    circle.setAttribute(
      "class",
      `point${p === selectedSrc ? " selected" : ""}`
    );
    circle.addEventListener("click", () => onPointClick(p));
    svg.appendChild(circle);

    if (occupiedWhite || occupiedBlack) {
      const stone = document.createElementNS(SVG_NS, "circle");
      stone.setAttribute("cx", String(x));
      stone.setAttribute("cy", String(y));
      stone.setAttribute("r", "16");
      stone.setAttribute("class", occupiedWhite ? "stone-w" : "stone-b");
      stone.style.pointerEvents = "none";
      svg.appendChild(stone);
    }
  }
}

function setStatus(text: string): void {
  statusEl.textContent = text;
}

function renderEvalPanel(text: string): void {
  evalListEl.textContent = text;
}

function renderMoveEvaluations(moves: Move[]): void {
  if (!evaluator) return;
  evalListEl.innerHTML = "";
  const rows = moves.map((mv) => {
    if ((countBits(mv.successorMover)) < 3) {
      return { mv, wdl: [0, 0, 1] as [number, number, number], depth: 0, immediate: true };
    }
    const e = evaluator!.evaluateTTA(mv.successorMover, mv.successorOpp);
    // e.wdlProbs is from the SUCCESSOR's mover perspective; flip to ours.
    const wdl: [number, number, number] = [e.wdlProbs[2], e.wdlProbs[1], e.wdlProbs[0]];
    return { mv, wdl, depth: e.depth, immediate: false };
  });
  rows.sort((a, b) => b.wdl[2] - a.wdl[2] + (b.wdl[1] - a.wdl[1]) * 0.01);

  for (const { mv, wdl, immediate } of rows) {
    const row = document.createElement("div");
    row.className = "move-row";
    const label = document.createElement("span");
    label.textContent = `${mv.src}→${mv.dst}${mv.captured !== null ? " x" + mv.captured : ""}`;
    label.style.width = "70px";
    const bar = document.createElement("div");
    bar.className = "bar";
    const l = document.createElement("div");
    l.className = "bar-loss";
    l.style.width = `${wdl[0] * 100}%`;
    const d = document.createElement("div");
    d.className = "bar-draw";
    d.style.width = `${wdl[1] * 100}%`;
    const w = document.createElement("div");
    w.className = "bar-win";
    w.style.width = `${wdl[2] * 100}%`;
    bar.append(l, d, w);
    row.append(label, bar);
    if (immediate) {
      const tag = document.createElement("span");
      tag.textContent = "(wins immediately)";
      row.appendChild(tag);
    }
    evalListEl.appendChild(row);
  }
}

function countBits(x: number): number {
  let c = 0;
  let m = x;
  while (m !== 0) {
    m &= m - 1;
    c++;
  }
  return c;
}

function checkGameOver(): boolean {
  if (isPlacementPhase(state)) return false;
  const term = terminalClass(mover(state), opp(state));
  if (term === WdlClass.Loss) {
    const loser = state.sideToMove;
    setStatus(`${loser === "white" ? "White" : "Black"} has no moves — ${otherSide(loser)} wins.`);
    return true;
  }
  return false;
}

async function maybeEngineMove(): Promise<void> {
  if (state.sideToMove === state.humanSide) return;
  if (checkGameOver()) return;

  await new Promise((r) => setTimeout(r, 150)); // small UX pause

  if (isPlacementPhase(state)) {
    const mv = chooseOpeningMove(mover(state), opp(state));
    if (!mv) return;
    applyPlacement(mv);
  } else {
    if (!net) return;
    const mv = chooseMove(net, mover(state), opp(state), { searchDepth: 2, rootTTA: true });
    if (!mv) return;
    applyMovement(mv);
  }
  render();
  await afterMove();
}

function applyPlacement(mv: PlacementMove): void {
  if (state.sideToMove === "white") state.whiteLeft--;
  else state.blackLeft--;
  setMoverOpp(state, mv.successorMover, mv.successorOpp);
  state.sideToMove = otherSide(state.sideToMove);
}

function applyMovement(mv: Move): void {
  setMoverOpp(state, mv.successorMover, mv.successorOpp);
  state.sideToMove = otherSide(state.sideToMove);
}

async function afterMove(): Promise<void> {
  if (checkGameOver()) return;

  if (isPlacementPhase(state)) {
    setStatus(
      `${state.sideToMove === "white" ? "White" : "Black"} to place ` +
        `(${state.whiteLeft} / ${state.blackLeft} left).`
    );
    renderEvalPanel("Placement phase is not model-backed (design-nn.md §10).");
  } else {
    setStatus(`${state.sideToMove === "white" ? "White" : "Black"} to move.`);
    if (state.sideToMove === state.humanSide) {
      renderMoveEvaluations(movesMovement(mover(state), opp(state)));
    }
  }

  await maybeEngineMove();
}

function onPointClick(p: number): void {
  if (awaitingCapture) {
    if (awaitingCapture.candidates.includes(p)) {
      awaitingCapture.resolve(p);
      awaitingCapture = null;
    }
    return;
  }
  if (state.sideToMove !== state.humanSide) return;

  if (isPlacementPhase(state)) {
    const moves = movesPlacement(mover(state), opp(state));
    const candidates = moves.filter((m) => m.to === p);
    if (candidates.length === 0) return;
    const chosen = resolveHumanCapture(candidates);
    chosen.then((mv) => {
      applyPlacement(mv);
      render();
      afterMove();
    });
    return;
  }

  const legal = movesMovement(mover(state), opp(state));
  if (selectedSrc === null) {
    if (((mover(state) >>> p) & 1) === 1) {
      selectedSrc = p;
      render();
    }
    return;
  }
  if (p === selectedSrc) {
    selectedSrc = null;
    render();
    return;
  }
  const candidates = legal.filter((m) => m.src === selectedSrc && m.dst === p);
  selectedSrc = null;
  if (candidates.length === 0) {
    render();
    return;
  }
  resolveHumanCapture(candidates).then((mv) => {
    applyMovement(mv);
    render();
    afterMove();
  });
}

function resolveHumanCapture<T extends { captured: number | null }>(candidates: T[]): Promise<T> {
  if (candidates.length === 1) return Promise.resolve(candidates[0]);
  setStatus("Mill! Click the opponent stone to capture.");
  return new Promise((resolve) => {
    const byPoint = new Map(candidates.map((c) => [c.captured as number, c]));
    awaitingCapture = {
      candidates: [...byPoint.keys()],
      resolve: (p) => resolve(byPoint.get(p)!),
    };
  });
}

async function init(): Promise<void> {
  const humanFirst = (document.getElementById("humanFirst") as HTMLInputElement).checked;
  state = newGame(humanFirst ? "white" : "black");
  selectedSrc = null;
  awaitingCapture = null;
  render();
  renderEvalPanel("Placement phase is not model-backed (design-nn.md §10).");
  setStatus("White to place.");

  if (!net) {
    try {
      net = await loadModel("../export");
      evaluator = new Evaluator(net);
      setStatus("Model loaded. White to place.");
    } catch (e) {
      setStatus("Could not load model from ../export — run `nmm export model` first.");
      console.error(e);
      return;
    }
  }

  await maybeEngineMove();
}

document.getElementById("resetBtn")!.addEventListener("click", () => void init());
void init();
