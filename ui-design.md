# Browser UI for Nine Men's Morris — Design

Goal: a **browser-based UI for playing Nine Men's Morris** against (or alongside) the
perfect-play engine, driven by the solved database produced by `ninemm solve`, with an
optional **evaluation overlay**: when a checkbox is set, the UI shows the
game-theoretic value of the current position and of every legal move.

This document is the "why and what". The step-by-step build plan for implementing it
is in [`ui-implementation.md`](ui-implementation.md). Background reading, in order:
[`readme-database.md`](readme-database.md) (value encoding, side-to-move convention),
[`design.md`](design.md) §6–§7 (opening search, perfect player),
[`readme-agent.md`](readme-agent.md) (the mover/opponent invariant — the single most
important thing to not get backwards).

---

## 1. Constraints that shape the design

1. **The database is large.** Sum over all 49 subspaces of `size(w,b) * 2` bytes ≈
   **17.1 GiB** (9,193,626,407 `u16` slots — computable from the canonical-set sizes
   in `readme-database.md` §4.1). It cannot be shipped to a browser; whatever consults
   it must run on the machine that holds the files.
2. **The database covers the movement phase only** (ply 19+). Placement-phase values
   (plies 1–18) require the alpha-beta opening search (`opening.rs`) probing the
   database at the phase boundary. That search is CPU- and RAM-adjacent work that also
   belongs next to the database, not in the browser.
3. **Everything hard is already implemented and oracle-verified in Rust**: rules
   (`movegen.rs`), canonicalization + indexing (`symmetry.rs`, `index.rs`), database
   loading (`persist.rs`, `retro::Database`), perfect move selection (`play.rs`),
   opening search (`opening.rs`). `readme-agent.md` documents three real bugs that
   survived code review and unit tests in exactly this logic. Reimplementing any of it
   in JavaScript would be inviting those bugs back with no oracle to catch them.
4. **Values are stored for the side to move**, not for a physical color. Every place
   where the UI shows "White wins in 13" is a perspective conversion away from a
   stored code. Concentrating those conversions in one tested place is a design goal.

## 2. Architecture: thin Rust server + rules-free browser client

```
┌────────────────────────┐   POST /api/analyze (JSON)   ┌──────────────────────────┐
│ Browser (static page)  │ ───────────────────────────► │ ninemm serve (Rust)      │
│ - SVG board, clicks    │                              │ - full db in RAM/mmap    │
│ - holds game state     │ ◄─────────────────────────── │ - movegen / index / play │
│ - zero game rules      │   legal moves + values +     │ - opening search + TT    │
└────────────────────────┘   engine choice + successors └──────────────────────────┘
```

A new `ninemm serve --dir db` subcommand embeds a small HTTP server in the existing
binary. It serves one static HTML page and one JSON endpoint. The browser client is a
single self-contained `index.html` (vanilla JS + SVG, no build step, no framework),
embedded into the binary with `include_str!` so there is nothing to deploy or path-configure.

**The client implements no game rules whatsoever.** For every position, the server
returns the complete list of legal moves, each with the full successor game state
attached. The client's only jobs are: draw the state, map clicks onto entries of the
move list, replace its state with the chosen move's `result`, and keep a history stack
for undo. This is the key simplification: rule bugs become impossible in the client
because the client has no rules, and the server's rules are the already-verified ones.

**The API is stateless.** The client sends the entire game state (stone sets, whose
turn, stones in hand) on every request; the server holds no per-game session. This
makes undo, "new game", page reload, and position setup trivial (all client-side), and
keeps the server a pure function `state → analysis` — easy to test without HTTP.

### Rejected alternatives

- **Pure static hosting + WASM/JS lookup with HTTP Range requests** into the `.bin`
  files. Feasible in principle (each probe reads 2 bytes at a computed offset, and
  `readme-database.md` is a complete spec), but it requires reimplementing
  canonicalization, combinatorial ranking, *and* full move generation in a second
  language with no oracle — exactly the risk class §1.3 warns about — and it still
  cannot do the placement phase (the opening search needs fast random access to the
  whole database, thousands of probes per evaluation). Not worth it for a UI whose
  database already lives on a workstation.
- **Compiling the Rust crate to WASM** and fetching the database into browser memory:
  17 GiB rules this out immediately.
- **A separate server in another language** (Python/Node reading the files per the
  spec): same reimplementation risk for canonicalization/indexing, plus a second
  toolchain. The Rust crate already has every needed function as a `pub` API.

## 3. Server design

### 3.1 Startup

Identical to the existing `play` command's preamble (reuse that code, factored into a
helper): load `manifest.json`, require all 49 subspaces present (partial databases
panic mid-search once the opening search or a capture probe needs a missing pair),
`mmap_subspace_verified` each file into a `retro::Database`. Add one development
escape hatch: `--allow-partial` skips the completeness check and makes the server
refuse placement-phase analysis (HTTP error) while still analyzing movement-phase
positions whose material pairs are present — this is what makes the implementation
testable against a fast `solve --max-total 7` database instead of the full 17 GiB one.

**Memory.** The server always memory-maps the subspace files read-only, verifying
each checksum with one streaming pass through the map (implemented as M6). Because a
UI session touches only a handful of values per move, the OS keeps just the touched
pages resident — and can drop them again under pressure — so the server runs
comfortably even on an 8 GiB machine. The original plan (load owned `Vec`s like
`play` once did, ~17.5 GiB, with mmap as an opt-in flag) turned out to be a trap:
on any machine without ~24 GiB free the load pushed the whole system into swap and
every analysis thrashed, which is exactly the "terribly slow or dead UI" failure
mode. `retro::Database` holds either owned or mapped storage, so everything
downstream of loading is unchanged.

**Threading: a small worker pool.** One user, one board, tiny request rate — but a
cold placement analysis can hold a request for seconds to minutes, and the original
single-threaded loop then blocked even `GET /` and `/api/meta`: reloading the page
mid-analysis looked like a dead server. A handful of synchronous worker threads
(still `tiny_http`, still no async runtime, matching the codebase's zero-async
style) each `recv()` from the shared server. The opening-search transposition table
sits behind a `Mutex` locked once per placement analysis, so placement analyses
still serialize — the TT-warming benefit is unchanged — while the page, `/api/meta`,
and movement-phase analyses stay responsive on the other workers. `--warm` runs
after the socket is bound, holding the TT lock, so the UI loads immediately during
warming and only placement analyses wait for it.

### 3.2 The one endpoint that matters: `POST /api/analyze`

Request = full game state in *physical-color* terms (the wire format never uses the
internal mover/opponent convention — that conversion happens exactly once, inside the
server):

```json
{
  "white": [6, 5, 13],          // point indices 0..23, spec §1 numbering
  "black": [9, 17, 21],
  "turn": "white",
  "whiteHand": 0,               // stones not yet placed
  "blackHand": 0,
  "evaluate": true,             // false ⇒ skip per-move values (perf, see §6)
  "engine": false               // request engineMove even without evaluate (see below)
}
```

Response:

```json
{
  "phase": "movement",                       // or "placement"
  "result": null,                            // or {"winner":"black","reason":"noMoves"|"fewerThanThree"}
  "value": {"outcome": "win", "plies": 13},  // side to move; plies null in placement phase
  "moves": [
    {
      "from": 6, "to": 7, "capture": 17,     // from/capture null when absent
      "notation": "a1-a4xd5",
      "value": {"outcome": "win", "plies": 13},   // outcome for the player MAKING the move
      "result": { "white": [...], "black": [...], "turn": "black",
                  "whiteHand": 0, "blackHand": 0 }
    }
  ],
  "engineMove": 0                            // index into moves[]; null if none
}
```

Semantics, pinned down (this is where every subtle bug lives):

- **Phase**: placement iff `whiteHand + blackHand > 0`. Placement legality/hand
  consistency is validated (White places on odd plies: `turn=="white" ⇒
  whiteHand==blackHand`; `turn=="black" ⇒ blackHand==whiteHand+1`).
- **Perspective conversion, in one place**: build the internal `Position` as
  `Position::new(white_bits, black_bits)` and, iff `turn=="black"`, `swap_colors()`.
  From that point on all internal code sees the standard mover/opponent form. Every
  successor `Position` coming back from `movegen`/`opening` is *itself*
  mover-flipped (its `.white()` is the *next* player's stones); converting successors
  back to physical colors reverses the same swap with the *next* turn.
- **Move values** (`evaluate: true`): for each legal move, the value is reported for
  the player making it. Movement phase: look up the successor's stored code `c`
  (successors that drop the opponent below 3 stones are an implicit `c = 0`, exactly
  as `play::best_movement_move` does); then `c == 0xFFFF` ⇒ draw, `c` even ⇒ **win in
  c+1** for the mover, `c` odd ⇒ **loss in c+1** for the mover. Placement phase:
  `opening::solve(successor)` gives the successor mover's three-valued outcome;
  negate it; no depth is available (the opening search is win/draw/loss only).
- **Position value**: movement phase — direct database lookup of the current
  position (or loss-in-0 if no legal moves); placement phase — `opening::solve` on
  the current state.
- **`engineMove`**: the index of the move `play::best_movement_move` /
  `play::best_placement_move` would pick (min-depth win ▸ any draw ▸ max-depth loss).
  In the movement phase it is always computed (a handful of probes). In the
  placement phase it is computed only when `evaluate` is true (derived from the
  per-move values at no extra cost) or the request sets `"engine": true` — an
  `evaluate:false` placement `engineMove` costs a full opening search (the
  early-exit on a first winning move never fires from drawn states, e.g. the empty
  board), so the client requests it only when the engine actually owns the side to
  move. Otherwise `engineMove` is null and the client, which ignores it on human
  turns anyway, loses nothing — this is what keeps the initial page load instant.
- **Game over**: movement — a side already below 3 stones, or the mover has no legal
  move; placement — the mover's `total` (board + hand) below 3. `moves` is empty and
  `result` set. (A player with stones in hand can never be blocked: at most 18 of 24
  points are ever occupied.)

### 3.3 Other routes

- `GET /` → the embedded `index.html`.
- `GET /api/meta` → `{ "complete": true, "subspaces": 49, "mmap": true }` so the UI
  can display database status and disable placement play against a partial database.

No CORS, no TLS, no auth: the server binds `127.0.0.1` by default (`--bind` to
override) and is a local analysis tool, same trust model as the `play` CLI.

## 4. Client design

One `ui/index.html` (HTML + CSS + JS in one file, embedded in the binary; served from
disk instead when `--ui-dir` is passed, for a fast edit-reload dev loop).

### 4.1 Board rendering

SVG, 7×7 logical grid using the standard a1–g7 coordinates (`readme-database.md` §1
gives the exact point-index → file/rank formula; the implementation guide contains the
derived 24-entry table). Lines for the three rings and four spokes; circles for the 24
points; filled discs for stones. Stones-in-hand shown as small discs beside the board.
Status line: whose turn, phase, and — on game over — the result.

### 4.2 Interaction

The client keeps `state` (the JSON game state), `analysis` (the last `/api/analyze`
response), and `history` (a stack of previous states for undo). After every state
change it re-requests analysis; all click handling is a *filter over
`analysis.moves`*:

- **Placement**: click an empty point ⇒ candidate moves are those with `to == point`.
- **Movement**: click one of your stones ⇒ remember `pendingFrom`, highlight the
  `to` points of moves with `from == pendingFrom`; click a highlighted point ⇒
  candidates are moves matching `(from, to)`.
- **Captures**: if the candidate set has more than one entry, the entries differ only
  in `capture`; highlight those opponent stones and wait for a click on one.
- When exactly one candidate remains: push current state onto `history`, set
  `state = candidate.result`, re-analyze.

Engine integration: a mode selector — *play White*, *play Black*, *two players /
analysis board*. When the side to move belongs to the engine, the client simply plays
`analysis.moves[analysis.engineMove]` (after a short delay so the move is visible).
Buttons: **New game**, **Undo** (pops two states when playing against the engine, one
otherwise). A collapsed "Set up position" panel exposes the state JSON in a textarea
with a Load button — a five-line feature that makes movement-phase testing possible
against a partial development database.

### 4.3 The evaluation overlay (the checkbox)

A checkbox **"Show evaluation"** controls `evaluate` in requests and all value
display. When off, nothing about the UI reveals values (so you can play a fair game).
When on:

- **Position banner**: the current position's value, translated to color terms:
  "White to move — wins in 13" / "draws" / "loses in 8" (placement phase: no ply
  count).
- **Per-move coloring** on the board, always from the mover's perspective:
  green = win, gray = draw, red = loss.
  - Placement: every empty point gets a colored dot = the **best** value among moves
    placing there (a mill-closing placement has one move per capture choice).
  - Movement, nothing selected: each of the mover's stones gets a colored ring = best
    value among its moves.
  - Movement, stone selected: each legal destination's dot shows that move's value,
    with the ply count as a small label.
  - Capture pending: each capturable stone shows the value of that capture choice.
- **Move list panel**: all legal moves with notation and value, sorted best-first
  (min-ply wins, then draws, then max-ply losses), engine's choice starred;
  click-to-play.

Ply counts are displayed as stored ("win in N plies", the database's depth coding per
`readme-database.md` §5). Colors + numbers, not colors alone (colorblind users get
the numbers and W/D/L letters).

### 4.4 Repetition

The database models repetition-as-draw implicitly (`design.md` §1): a position that
cycles under optimal play is stored as a draw, so the evaluation display is already
correct without any repetition logic. For *finishing* a game between imperfect
players, though, the client keeps a count of `(white, black, turn)` occurrences in
the movement phase and declares a draw on the third repetition. Client-side only; the
server stays stateless.

## 5. Value display correctness — the test that matters

The mover/opponent convention gives this project's UI its one really dangerous bug
class: values shown for the wrong side. Two cheap, strong checks are mandated:

1. **Color-swap invariance test** (server, automated): for random states, analyzing
   `(white=A, black=B, turn=white)` and `(white=B, black=A, turn=black)` must produce
   identical analyses modulo relabeling. Any perspective slip breaks this.
2. **Self-consistency test** (server, automated, against a `--max-total 7` dev
   database): for every reported move value "win in d", the successor's own analysis
   must report "loss in d−1" for the other side (and draws map to draws) — the same
   invariant `play.rs`'s self-play soak test enforces.

## 6. Performance

- **Movement phase**: an analysis is ~(number of legal moves) database probes — a few
  dozen 2-byte reads (RAM) or page-cache hits (mmap). Effectively instant.
- **Placement phase with `evaluate:true`** runs the alpha-beta search once per legal
  move (up to 24 at ply 1) with no early cutoff — this is the expensive case, seconds
  to potentially minutes cold. Mitigations, in order of impact: the transposition
  table **lives for the whole server process** (every analysis warms every later
  one; the same table is what makes `play`'s engine fast after its first move);
  `--warm` optionally runs the empty-board solve at startup so even the first ply-1
  analysis is instant; `evaluate:false` (checkbox off) uses the early-cutoff path
  when the engine owns the turn, and runs **no search at all** on human-owned turns
  (the client omits `"engine": true`, §3.2 — move enumeration is then instant and
  doesn't even wait on the TT lock). The UI shows a busy indicator during analysis
  and disables input. TT growth is
  bounded in practice (entries are 11-byte key/value pairs; a long analysis session
  stays in the hundreds of MB) — acceptable for a workstation tool; not persisted.
- The engine "thinks" on the server synchronously; placement analyses serialize on
  the transposition-table lock, so a slow analysis delays the user's next *placement
  analysis* — but never the page itself, `/api/meta`, or movement-phase requests
  (worker pool, §3.1 Threading).

## 7. Out of scope (deliberately)

- Multiple concurrent games / sessions, auth, remote deployment hardening.
- Opening *depth* display (would need a full opening database — `design.md` §9).
- Ultra-strong move choice among equal-value moves (readme-agent.md "not implemented").
- Mobile-first layout, animations beyond basic move highlighting, sound.
- Persisting the opening transposition table to disk between server runs.

## 8. Risks

| Risk | Mitigation |
|---|---|
| Perspective (mover vs. color) slip in server or UI | single conversion point (§3.2); mandatory tests (§5); wire format is always physical-color |
| Client and server rules drift | client has no rules — it can only play moves the server enumerated, and applies server-provided successor states |
| First placement analysis feels hung | process-lifetime TT, `--warm`, busy indicator, `evaluate:false` fast path |
| 17.5 GiB RAM unavailable | mmap is the only load path (M6): just the touched pages stay resident; `--allow-partial` + position setup for development |
| Partial dev database panics mid-search | completeness check at startup identical to `play`; `--allow-partial` restricts to movement-phase pairs that are present and rejects placement analysis |
