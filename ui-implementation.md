# Browser UI — Implementation Guide

Step-by-step build plan for the architecture in [`ui-design.md`](ui-design.md). It is
written to be executable top-to-bottom without further design decisions: every
milestone lists the files to touch, the exact existing APIs to call, code for the
error-prone parts, and a "Done when" gate. Follow the milestones in order and commit
after each one (`cargo fmt && cargo clippy --release --all-targets && cargo test
--release` must be clean before every commit).

**Read first**: `readme-database.md` §2 and §5 (side-to-move convention, value
codes), `readme-agent.md` "The single most important invariant". You will not need to
modify any solver code (`retro.rs`, `index.rs`, `symmetry.rs`, `movegen.rs`,
`opening.rs`, `play.rs`) except the small, contained `retro::Database` storage change
in optional milestone M6 — if you find yourself editing solver internals anywhere
else, stop and re-read the relevant milestone; the design deliberately avoids it.

**Ground rules** (from `ui-design.md`, repeated because they prevent whole bug
classes):

1. The wire format (JSON) always uses **physical colors** (white/black + whose turn).
   The internal mover/opponent `Position` convention appears only inside
   `src/server.rs`, converted at exactly one entry point and one exit point (given
   below as `to_internal` / `to_state_json`). Never mix the two.
2. The browser client contains **zero game rules**. It only renders state, filters
   the server-provided move list by clicks, and adopts server-provided successor
   states. If you are writing mill detection or adjacency in JavaScript, you have
   gone off the plan.
3. All values shown anywhere are **for the side to move** in the position being
   displayed, translated to color words ("White wins in 13") only at render time.

**Development database**: the full database is ~17 GiB and takes hours to solve. For
all development and automated tests, build the small one:

```sh
cargo build --release
./target/release/ninemm solve --dir devdb --max-total 7   # {3,3} and {4,3}; ~seconds/minutes
```

Run the server against it with `--allow-partial`, and use the UI's "Set up position"
panel (M3) with 3-vs-3 / 4-vs-3 movement positions. Placement-phase behavior can only
be exercised against a complete database (M5's manual checklist).

---

## M0 — `serve` skeleton

**Files**: `Cargo.toml`, `src/main.rs`, `src/lib.rs`, new `src/server.rs`, new
`ui/index.html`.

1. Add the HTTP dependency to `Cargo.toml` (no other new dependencies are needed in
   the whole project; `serde`/`serde_json` are already present):

   ```toml
   tiny_http = "0.12"
   ```

2. Add `pub mod server;` to `src/lib.rs` (alphabetical position among the existing
   `pub mod` lines).

3. Create `ui/index.html` with a placeholder page (`<h1>ninemm</h1>`); it gets real
   content in M3.

4. Create `src/server.rs`:

   ```rust
   //! HTTP server for the browser UI (ui-design.md): one static page and a
   //! stateless JSON analysis endpoint over the solved database.

   use crate::retro::Database;
   use anyhow::Result;
   use std::path::Path;

   const INDEX_HTML: &str = include_str!("../ui/index.html");

   pub struct ServeOptions {
       pub bind: String,          // e.g. "127.0.0.1:8080"
       pub allow_partial: bool,
       pub warm: bool,            // used from M5
       pub ui_dir: Option<std::path::PathBuf>, // used from M5
   }

   pub fn serve(dir: &Path, opts: &ServeOptions) -> Result<()> {
       let (db, complete) = load_db(dir, opts.allow_partial)?;
       let server = tiny_http::Server::http(&opts.bind)
           .map_err(|e| anyhow::anyhow!("failed to bind {}: {e}", opts.bind))?;
       println!("Serving on http://{}", opts.bind);
       let mut tt = std::collections::HashMap::new(); // opening-search TT, process lifetime
       for mut req in server.incoming_requests() {
           let response = route(&mut req, &db, complete, opts, &mut tt);
           let _ = req.respond(response); // client hung up: ignore, keep serving
       }
       Ok(())
   }
   ```

   `load_db` replicates the loading preamble of `run_play` in `src/main.rs` (manifest
   load, completeness check over all 49 `(w,b)` for `w,b in 3..=9`,
   `persist::read_subspace_verified` into `Database`), with two changes: when
   `allow_partial` is false, a missing subspace is a hard error exactly as in
   `run_play`; when true, load whatever subspaces the manifest has and return
   `complete = false`. Return `(Database, bool)`.

   `route` matches `(req.method(), req.url())`:
   - `GET /` → 200, `INDEX_HTML`, header `Content-Type: text/html; charset=utf-8`.
   - `GET /api/meta` → 200, JSON `{"complete": <bool>, "subspaces": <count>,
     "mmap": false}`.
   - `POST /api/analyze` → stub for now: 501 with JSON `{"error":"not implemented"}`.
   - anything else → 404, JSON `{"error":"not found"}`.

   Helper for JSON responses (reuse everywhere):

   ```rust
   fn json_response(status: u16, body: String) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
       let header = tiny_http::Header::from_bytes(
           &b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap();
       tiny_http::Response::from_string(body).with_status_code(status).with_header(header)
   }
   ```

   Read a POST body with `req.as_reader().read_to_string(&mut buf)?` (cap at, say,
   64 KiB; a longer body is a 400).

5. Wire the subcommand into `src/main.rs`, following the style of the existing
   variants:

   ```rust
   /// Serve the browser UI and JSON analysis API over the solved database.
   Serve {
       #[arg(long, default_value = "db")]
       dir: PathBuf,
       /// Address to bind (local analysis tool; keep it on localhost).
       #[arg(long, default_value = "127.0.0.1:8080")]
       bind: String,
       /// Load whatever subspaces exist instead of requiring all 49.
       /// Placement-phase analysis is refused; movement-phase analysis
       /// works for material pairs that are present. Development aid.
       #[arg(long)]
       allow_partial: bool,
       /// Run the empty-board opening solve at startup to warm the
       /// transposition table (first placement analysis becomes instant).
       #[arg(long)]
       warm: bool,
       /// Serve ui/index.html from this directory instead of the copy
       /// embedded at compile time (edit-reload development loop).
       #[arg(long)]
       ui_dir: Option<PathBuf>,
   }
   ```

   The handler just builds `ServeOptions` and calls `ninemm::server::serve`.

**Done when**: against `devdb`,
`./target/release/ninemm serve --dir devdb --allow-partial` starts;
`curl http://127.0.0.1:8080/` returns the placeholder page;
`curl http://127.0.0.1:8080/api/meta` returns `{"complete":false,"subspaces":3,...}`
(3 = subspaces `3-3`, `4-3`, `3-4`); without `--allow-partial` it exits with the
same clear "incomplete database" error `play` gives. Commit.

## M1 — `/api/analyze`, movement phase

All in `src/server.rs`. This milestone implements the full analysis response for
states with `whiteHand == blackHand == 0`, plus request validation. Placement states
return a 501 JSON error until M2.

### 1.1 Wire types

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Color { White, Black }
impl Color { fn other(self) -> Color { match self { Color::White => Color::Black, Color::Black => Color::White } } }

/// A game state in physical-color terms — the only shape that crosses the wire.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StateJson {
    pub white: Vec<u8>,      // point indices 0..=23
    pub black: Vec<u8>,
    pub turn: Color,
    pub white_hand: u8,
    pub black_hand: u8,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeRequest {
    #[serde(flatten)]
    pub state: StateJson,
    #[serde(default)]
    pub evaluate: bool,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Outcome { Win, Draw, Loss }

#[derive(Serialize, Clone, Copy)]
pub struct ValueJson { pub outcome: Outcome, pub plies: Option<u16> }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveJson {
    pub from: Option<u8>,
    pub to: u8,
    pub capture: Option<u8>,
    pub notation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ValueJson>,   // present iff request had evaluate:true
    pub result: StateJson,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultJson { pub winner: Color, pub reason: &'static str } // "fewerThanThree" | "noMoves"

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResponse {
    pub phase: &'static str,               // "placement" | "movement"
    pub result: Option<ResultJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ValueJson>,          // absent when game over or evaluate:false
    pub moves: Vec<MoveJson>,
    pub engine_move: Option<usize>,
}
```

### 1.2 Validation and perspective conversion

Implement `analyze(req: &AnalyzeRequest, db: &Database, complete: bool, tt: &mut Tt)
-> Result<AnalyzeResponse, ApiError>` as a **pure function** (no HTTP types) so it
can be unit-tested directly. `ApiError` is `{ status: u16, message: String }`;
`type Tt = std::collections::HashMap<(Position, u8, u8), i8>`.

Validation, in order (each failure → status 400 with a specific message):

- every point index `< 24`; no duplicates within or across `white`/`black`
  (build `u32` bitmasks while checking: `white_bits`, `black_bits`);
- `white_hand <= 9`, `black_hand <= 9`,
  `white.len() + white_hand as usize <= 9`, same for black;
- placement-alternation consistency when any hand is nonzero:
  `turn == White ⇒ white_hand == black_hand`,
  `turn == Black ⇒ black_hand == white_hand + 1`.

Conversion — the only two functions where colors and mover/opponent meet:

```rust
use crate::pos::Position;

/// Physical colors -> internal mover/opponent Position.
fn to_internal(white_bits: u32, black_bits: u32, turn: Color) -> Position {
    let p = Position::new(white_bits, black_bits);
    match turn { Color::White => p, Color::Black => p.swap_colors() }
}

/// Internal mover/opponent Position (whose mover is the player `turn`)
/// -> physical-color wire state.
fn to_state_json(pos: Position, turn: Color, white_hand: u8, black_hand: u8) -> StateJson {
    let phys = match turn { Color::White => pos, Color::Black => pos.swap_colors() };
    StateJson {
        white: crate::pos::bits(phys.white()).map(|p| p as u8).collect(),
        black: crate::pos::bits(phys.black()).map(|p| p as u8).collect(),
        turn, white_hand, black_hand,
    }
}
```

### 1.3 Game-over detection (movement)

With `pos = to_internal(...)` (mover perspective; `pos.white()` is the stones of the
player `turn`):

- `pos.white_count() < 3` → `result = { winner: turn.other(), reason: "fewerThanThree" }`.
- else `pos.black_count() < 3` → `result = { winner: turn, reason: "fewerThanThree" }`.
- else if `movegen::successors(pos)` is empty → `result = { winner: turn.other(),
  reason: "noMoves" }`.

On game over: `moves` empty, `value` and `engine_move` `None`, return early.

### 1.4 Partial-database guard

Before any lookup, when `!complete`, check that every subspace a movement analysis
can touch is loaded, and 409 with a clear message otherwise. With `w =
pos.white_count() as usize`, `b = pos.black_count() as usize`, the needed subspaces
are `(w, b)` (the position itself), `(b, w)` (quiet successors), and — only if `b > 3`
— `(b - 1, w)` (capture successors; a capture from `b == 3` leaves fewer than 3
stones, which is never looked up — see 1.5). Use `db.has(w, b)`. With a complete
database, skip the check.

### 1.5 Moves, values, engine choice

```rust
use crate::{board, movegen, play, retro};

let succs = movegen::successors(pos);
let mut moves = Vec::with_capacity(succs.len());
for &succ in &succs {
    // succ is mover-flipped: succ.black() is the CURRENT mover's stones
    // after the move; succ.white() is the opponent's (post-capture).
    let from_bits = pos.white() & !succ.black();     // exactly one bit
    let to_bits = succ.black() & !pos.white();       // exactly one bit
    let cap_bits = pos.black() & !succ.white();      // zero or one bit
    debug_assert_eq!(from_bits.count_ones(), 1);
    debug_assert_eq!(to_bits.count_ones(), 1);
    debug_assert!(cap_bits.count_ones() <= 1);
    let from = from_bits.trailing_zeros() as u8;
    let to = to_bits.trailing_zeros() as u8;
    let capture = (cap_bits != 0).then(|| cap_bits.trailing_zeros() as u8);

    let value = req.evaluate.then(|| movement_move_value(succ, db));
    moves.push(MoveJson {
        from: Some(from), to, capture,
        notation: movement_notation(from, to, capture),
        value,
        result: to_state_json(succ, turn.other(), 0, 0),
    });
}
```

The value mapping — copy this exactly; it is the same rule `play::best_movement_move`
applies internally (see `readme-database.md` §5):

```rust
/// Value of making a move, for the player making it, given the successor
/// position (which is from the NEXT mover's perspective).
fn movement_move_value(succ: Position, db: &Database) -> ValueJson {
    // Opponent reduced below 3 stones: an implicit loss-in-0 for the next
    // mover, not stored in any file.
    let code = if succ.white_count() < 3 { 0 } else { db.lookup_pos(succ) };
    if code == retro::DRAW {
        ValueJson { outcome: Outcome::Draw, plies: None }
    } else if code % 2 == 0 {
        ValueJson { outcome: Outcome::Win, plies: Some(code + 1) }  // their loss in c = our win in c+1
    } else {
        ValueJson { outcome: Outcome::Loss, plies: Some(code + 1) } // their win in c = our loss in c+1
    }
}

fn movement_notation(from: u8, to: u8, capture: Option<u8>) -> String {
    let mut s = format!("{}-{}", board::point_name(from as usize), board::point_name(to as usize));
    if let Some(c) = capture { s.push('x'); s.push_str(&board::point_name(c as usize)); }
    s
}
```

Position value (only when `req.evaluate`): `let code = db.lookup_pos(pos);` then
`DRAW` → draw / even → **loss in `code`** / odd → **win in `code`** — note this is
the mirror of the successor mapping: here the code already belongs to the side to
move, no `+1`, no parity flip.

Engine move — do not re-derive the selection rule; reuse the tested one and map its
choice back to an index by position equality (distinct legal moves always produce
distinct successor positions, so the match is unambiguous):

```rust
let engine_move = play::best_movement_move(pos, db)
    .map(|c| succs.iter().position(|s| *s == c.successor).expect("choice comes from succs"));
```

### 1.6 HTTP glue

Replace M0's 501 stub: parse the body with `serde_json::from_str::<AnalyzeRequest>`
(parse failure → 400 with the serde error text), call `analyze`, serialize with
`serde_json::to_string`. Map `ApiError` to its status with `{"error": message}`.

### 1.7 Tests (same file, `#[cfg(test)] mod tests`)

Follow the pattern of `src/play.rs`'s tests: build a temp database with
`orchestrate::solve_all(&tmp, Some(7))` once per test (they already tolerate the
~seconds cost), load it with the same helper `load_db` uses. Write:

1. **Validation**: duplicate point → 400; overlapping white/black → 400;
   `white.len() + white_hand > 9` → 400; bad hand alternation → 400.
2. **Game over**: a state with 2 white stones, turn white → `result.winner == Black`,
   reason `fewerThanThree`, empty moves. A **blocked** state: find one by scanning
   `SubspaceId::new(4, 3)` — iterate `idx`, keep `index::is_canonical_slot(sub, idx)`,
   `let pos = index::unindex(sub, idx)`, take the first with `pos.is_blocked()`;
   convert to wire form (turn white: `white = bits(pos.white())`,
   `black = bits(pos.black())`) → reason `noMoves`.
3. **Color-swap invariance** (the perspective test — do not skip): for ~200 canonical
   slots sampled across `{3,3}` and `{4,3}` (step through indices like
   `play.rs`'s tests do), analyze `A = (white=W, black=B, turn=white)` and
   `B = (white=B, black=W, turn=black)` with `evaluate:true`. Assert: identical
   `phase`, `result`, `value`, `engine_move`, same length `moves`, and per-move
   identical `from`/`to`/`capture`/`value`, with `B`'s move results equal to `A`'s
   after swapping the `white`/`black` arrays and flipping `turn`.
4. **Self-consistency** (the depth test): for the same sample, for every move with
   `value = win in d`: analyze `move.result`; its position `value` must be
   **loss in d−1** (or the result must be game over with the mover as loser when
   `d == 1`). Draw moves → successor value draw. Loss-in-d moves → successor value
   win in d−1.
5. **Partial guard**: a 5-vs-5 movement state against the `--max-total 7` database →
   409.

**Done when**: all tests green; `curl`-ing a hand-written 3-vs-3 state at the running
server returns sensible moves and values that agree with `db-stats` expectations
(e.g. most 3-3 positions are wins for the side to move). Commit.

## M2 — `/api/analyze`, placement phase

Still `src/server.rs`. Placement states are those with `white_hand + black_hand > 0`
(validation from M1 already ensured hand consistency).

1. **Refuse on partial databases**: if `!complete` → 409
   `"placement analysis requires the complete 49-subspace database"` (the opening
   search can reach nearly any material pair; see `getting-started.md`).

2. **Build the internal state.** `opening::PlacementState` is mover-relative,
   including the hands:

   ```rust
   use crate::opening::{self, PlacementState};
   let pos = to_internal(white_bits, black_bits, turn);
   let (mover_hand, opp_hand) = match turn {
       Color::White => (req.state.white_hand, req.state.black_hand),
       Color::Black => (req.state.black_hand, req.state.white_hand),
   };
   let ps = PlacementState { pos, mover_hand, opp_hand };
   ```

3. **Game over**: `ps.total_mover() < 3` → winner `turn.other()`, reason
   `fewerThanThree`; else `ps.total_opp() < 3` → winner `turn`, same reason. (A
   player with stones in hand always has a legal placement — at most 18 of 24 points
   are occupied — so `noMoves` cannot happen here.)

4. **Moves.** `opening::successors(&ps)` returns flipped states (`succ.pos.black()`
   is the current mover's stones incl. the new one; `succ.mover_hand` is the *next*
   mover's hand — see its doc comment). Per successor `s`:

   ```rust
   let to_bits = s.pos.black() & !ps.pos.white();    // exactly one bit
   let cap_bits = ps.pos.black() & !s.pos.white();   // zero or one bit
   ```

   `from` is `None`; notation is `"d5"` / `"d5xb4"`. The result state's hands map
   back to colors via the *next* turn:

   ```rust
   let next = turn.other();
   let (wh, bh) = match next {
       Color::White => (s.mover_hand, s.opp_hand),
       Color::Black => (s.opp_hand, s.mover_hand),
   };
   let result = to_state_json(s.pos, next, wh, bh);
   ```

5. **Values** (`evaluate:true`): the opening search is three-valued, no ply counts.

   ```rust
   let v = opening::solve(&s, db, tt);        // outcome for the NEXT mover
   let value = ValueJson {
       outcome: match v {                     // negate: their win is our loss
           opening::Value::Win => Outcome::Loss,
           opening::Value::Draw => Outcome::Draw,
           opening::Value::Loss => Outcome::Win,
       },
       plies: None,
   };
   ```

   Position value: `opening::solve` on a `PlacementState` equal to `ps` (same
   mapping, *without* negation — it is already the mover's value).

6. **Engine move.** When `evaluate:true`, pick from the values already computed
   (first move whose value is `Win`, else first `Draw`, else index 0) — do not run
   the search twice. When `evaluate:false`, call `play::best_placement_move(&ps, db,
   tt)` (it early-exits on the first win, the fast path) and map back by equality on
   the returned `PlacementState` against the successor list.

7. **Tests** (runnable without the full database):
   - a placement request against the partial dev database → 409;
   - descriptor extraction: for `PlacementState::initial()` and a couple of
     hand-built mid-placement states, check — without any database — that
     `opening::successors` diffing yields `from == None`, a `to` that was empty, and
     result hands per the alternation rule. (Skip value/engine assertions; those
     need the full database and are covered by M5's manual checklist.)

**Done when**: tests green; against a **full** database (if available)
`curl` on the empty board (`white:[], black:[], turn:"white", whiteHand:9,
blackHand:9, evaluate:true`) returns 24 moves and a `draw` position value —
reproducing the headline result through the API. Commit.

## M3 — Frontend: board and play (no evaluation yet)

**File**: `ui/index.html` — one self-contained file: `<style>`, an SVG board, a side
panel, one `<script>`. No frameworks, no build step. Also implement the `--ui-dir`
option in `src/server.rs` now (read `index.html` from that directory per request when
set, else use `INDEX_HTML`) — it makes the rest of this milestone a refresh-only
loop: `./target/release/ninemm serve --dir devdb --allow-partial --ui-dir ui`.

### 3.1 Geometry

Point index → board coordinates, derived once from `readme-database.md` §1 (columns
`a..g` = 1..7). Copy this table verbatim; do not re-derive it:

```js
// COORD[p] = [col, row], 1..7 each; name = "abcdefg"[col-1] + row
const COORD = [
  [1,7],[4,7],[7,7],[7,4],[7,1],[4,1],[1,1],[1,4],   // outer ring, p=0..7  (a7 d7 g7 g4 g1 d1 a1 a4)
  [2,6],[4,6],[6,6],[6,4],[6,2],[4,2],[2,2],[2,4],   // middle ring, p=8..15 (b6 d6 f6 f4 f2 d2 b2 b4)
  [3,5],[4,5],[5,5],[5,4],[5,3],[4,3],[3,3],[3,4],   // inner ring, p=16..23 (c5 d5 e5 e4 e3 d3 c3 c4)
];
const X = c => 40 + (c - 1) * 70;          // SVG viewBox 0 0 500 500
const Y = r => 40 + (7 - r) * 70;
const NAME = p => "abcdefg"[COORD[p][0] - 1] + COORD[p][1];
```

Board lines: for each ring `r` in `0..3`, a closed polygon through points
`r*8 + 0 .. r*8 + 7` in order; plus four spoke lines connecting `COORD[0*8+i]` to
`COORD[2*8+i]` for `i` in `{1,3,5,7}`. Then 24 small circles (the points, clickable,
generous `r=18` invisible hit area) and, per state render, stone discs (white fill
`#f2f2f2` / black fill `#333`, both with a dark stroke). Stones in hand: two rows of
small discs beside the board, one per `whiteHand`/`blackHand`.

### 3.2 Client state

```js
let state = initialState();     // {white:[],black:[],turn:"white",whiteHand:9,blackHand:9}
let analysis = null;            // last /api/analyze response
let history = [];               // stack of previous states (undo)
let pendingFrom = null, pendingTo = null;  // click-selection progress
let mode = "white";             // "white" | "black" | "both" — sides the HUMAN plays
let showEval = false;           // the checkbox (wired in M4; keep the variable now)
let busy = false;
let repetition = new Map();     // M4
```

`async function refresh()`: set `busy` (dim board, block clicks), `POST
/api/analyze` with `{...state, evaluate: showEval}`, store `analysis`, clear
`pendingFrom/To`, render, then: if `!analysis.result` and the side to move is the
engine's (`mode !== "both" && state.turn !== mode`) and `analysis.engineMove !=
null`, `setTimeout(() => applyMove(analysis.moves[analysis.engineMove]), 400)`.
Non-2xx responses: show `body.error` in the status area (this is how partial-database
placement refusal surfaces) and leave the position unchanged.

`function applyMove(m)`: `history.push(state); state = m.result; refresh();`.
That is the entire move application — no rules.

### 3.3 Click handling — a filter over `analysis.moves`

```js
function candidates() {
  let ms = analysis.moves;
  if (pendingFrom != null) ms = ms.filter(m => m.from === pendingFrom);
  if (pendingTo != null) ms = ms.filter(m => m.to === pendingTo);
  return ms;
}
function onPointClick(p) {
  if (busy || !analysis || analysis.result) return;
  if (mode !== "both" && state.turn !== mode) return;      // engine's turn
  const phase = analysis.phase;
  if (pendingTo != null) {                                  // awaiting capture choice
    const m = candidates().find(m => m.capture === p);
    if (m) applyMove(m); else resetSelection();
    return;
  }
  if (phase === "movement" && pendingFrom == null) {
    if (analysis.moves.some(m => m.from === p)) { pendingFrom = p; render(); }
    return;
  }
  // placement click, or movement destination click
  const ms = analysis.moves.filter(m =>
      (phase === "placement" || m.from === pendingFrom) && m.to === p);
  if (ms.length === 1) applyMove(ms[0]);
  else if (ms.length > 1) { pendingTo = p; render(); }      // mill: choose capture
  else resetSelection();
}
```

Highlights during selection: selected stone gets a blue ring; legal destinations
(`candidates().map(m => m.to)`) get small dots; when `pendingTo` is set, capturable
stones (`candidates().map(m => m.capture)`) get dashed red rings.

### 3.4 Panel

- Status line: phase, "White/Black to move", or on `analysis.result`:
  "White wins (opponent has fewer than three stones)" / "(no legal moves)".
- Mode selector (three radio buttons: play White / play Black / two players);
  changing it just calls `refresh()` (the engine autoplay condition re-evaluates).
- **New game** (`state = initialState(); history = []; repetition.clear(); refresh()`),
  **Undo** — pop one state, or two when playing against the engine and it is the
  human's turn (so undo takes back the human move *and* the engine's reply); disabled
  while `busy` or when `history` is empty.
- **Set up position** (a `<details>` element): textarea prefilled with
  `JSON.stringify(state, null, 1)` + a Load button that parses it, replaces `state`,
  clears history/repetition, and refreshes; server-side validation errors from
  `refresh()` appear in the status area. This panel is the movement-phase testing
  path against the dev database, e.g.
  `{"white":[0,9,17],"black":[3,12,21],"turn":"white","whiteHand":0,"blackHand":0}`.
- On load, `GET /api/meta`; if `!complete`, show a persistent banner: "Partial
  database — placement play unavailable; use Set up position".

**Done when** (manual, against `devdb --allow-partial --ui-dir ui`): loading a 3-vs-3
position via the setup panel renders it; human moves work including a mill capture
(set up a position one move from a mill); the engine answers when it owns a side;
undo and new-game behave; illegal clicks do nothing. Commit.

## M4 — Frontend: evaluation overlay + repetition

1. **Checkbox** "Show evaluation" bound to `showEval`; toggling calls `refresh()`
   (the request's `evaluate` flag changes, so per-move values appear/disappear).
   When off, render no value information at all.

2. **Value helpers** (mover's perspective; used everywhere):

   ```js
   const EVAL_COLOR = { win: "#2e7d32", draw: "#9e9e9e", loss: "#c62828" };
   // Sort key: quick wins first, then draws, then slow losses.
   function rank(v) {
     if (v.outcome === "win")  return 0 * 1000 + (v.plies ?? 0);
     if (v.outcome === "draw") return 1 * 1000;
     return 2 * 1000 + (1000 - (v.plies ?? 0));
   }
   function valueText(v, side) {  // side = "White" | "Black" (the mover)
     const n = v.plies != null ? ` in ${v.plies}` : "";
     return v.outcome === "draw" ? `${side} to move — draw`
          : `${side} to move — ${v.outcome === "win" ? "wins" : "loses"}${n}`;
   }
   ```

3. **Banner**: when `showEval` and `analysis.value` exists, show
   `valueText(analysis.value, state.turn)`.

4. **Board overlay** (only when `showEval`), per the selection stage:
   - placement, nothing pending: on each empty point that is some move's `to`, a dot
     colored by the **best** (`rank`-minimal) value among moves with that `to`;
   - movement, nothing selected: on each stone that is some move's `from`, a ring
     colored by the best value among that stone's moves;
   - `pendingFrom` set: each destination dot takes its move's color plus a tiny text
     label of `plies` (best over capture variants when several moves share the `to`);
   - `pendingTo` set (capture choice): each capturable stone's dashed ring takes that
     specific move's value color.

5. **Move list panel**: when `showEval`, list `analysis.moves` sorted by
   `rank(m.value)` — notation, value text (`W13` / `D` / `L8` compact form), a star
   on `analysis.engineMove`'s entry; click plays the move (respect the same
   human-turn guard as board clicks).

6. **Repetition draw** (movement phase only, client-side): after each `applyMove`,
   when both hands are 0, increment `repetition` under the key
   `JSON.stringify([[...state.white].sort((a,b)=>a-b), [...state.black].sort((a,b)=>a-b), state.turn])`.
   On a count reaching 3, declare "Draw by threefold repetition" in the status area
   and stop engine autoplay (a local game-over flag the renderer and click guards
   respect; server analyses continue to work if the user keeps exploring). New
   game / setup / undo rebuild or clear the map (on undo, decrement or simply rebuild
   from `history` — rebuilding is simpler and obviously correct).

**Done when** (manual, dev database, 3-vs-3 setups): with the checkbox on, banner and
per-move colors appear and agree with the move list; toggling off removes every
trace; walking a forced win shows the banner count down (win in 13 → opponent loses
in 12 → win in 11 …); shuffling a stone back and forth triggers the repetition draw
at the third occurrence. Commit.

## M5 — Polish and full-database verification

1. **`--warm`**: after loading a complete database, run
   `opening::solve(&PlacementState::initial(), &db, &mut tt)` with the server's
   long-lived `tt` before accepting requests, printing the root value (must be
   `Draw`) and the elapsed time. Skip with a note when the database is partial.
2. **Busy UX check**: the first un-warmed placement analysis can take minutes —
   confirm the busy indicator covers it and input stays blocked (no re-entrant
   requests).
3. **README**: add `serve` to the command list in `README.md`'s repository-layout
   section and a short "Browser UI" subsection to `getting-started.md` (run command,
   the checkbox, `--warm`, RAM note, `--mmap` once M6 lands), linking `ui-design.md`.
4. **Manual checklist against the full database** (record the outcomes in the commit
   message):
   - `serve --dir db --warm` starts, reports root value **draw**;
   - empty board, checkbox on: position banner "White to move — draw"; every
     first-placement value is win/draw/loss-colored with no error;
   - play a full game vs. the engine in each mode; placement→movement transition is
     seamless; mill captures prompt correctly in both phases;
   - a deliberately thrown-away game shows the engine's advantage climbing (loss
     depths shrinking); engine converts a won endgame;
   - kill and restart the server mid-game: the client's next request re-analyzes the
     same state correctly (statelessness check);
   - checkbox off for one full game: no value leakage anywhere.

**Done when**: checklist passes; docs updated; clippy/fmt/tests clean. Commit.

## M6 — mmap loading for `serve` (implemented; it is the default, not a flag)

Originally planned as an optional `--mmap` flag; shipped as the *only* load path
after the owned-`Vec` default proved unusable in practice: on an 18 GiB machine the
17 GiB load pushed the whole system into swap, startup kept the port closed for
minutes (connection refused), and every later analysis thrashed — the UI read as
"terribly slow or dead". Two server-side changes landed with it, beyond the original
plan:

- **Bind before `--warm`.** The socket now opens before the empty-board warm solve;
  the warm solve runs holding the TT lock, so the page loads instantly and only
  placement analyses wait for warming to finish.
- **Worker pool instead of the single request loop.** A few `tiny_http` worker
  threads `recv()` from the shared server; the TT is behind a `Mutex` locked once
  per placement analysis (movement analyses never touch it). A slow placement
  analysis can no longer block `GET /` or `/api/meta`. See ui-design.md §3.1
  "Threading".

Only milestone that touches solver code; the change is confined to storage plumbing
(no algorithmic code), but still follow `readme-agent.md`: run the full release test
suite afterwards, and if anything in `retro.rs` beyond the `Database` struct seems to
need changing, stop — it does not.

1. In `retro.rs`, generalize `Database`'s per-subspace storage. Shipped as the
   `Backing` enum (`Owned(Vec<u16>)` / `Mapped(memmap2::Mmap)`) with per-index
   access through `persist::mmap_get_u16` rather than an unsafe `as_slice()` view.
   `insert(w, b, values: Vec<u16>)` is kept (every existing call site compiles
   unchanged) alongside `insert_mmap(w, b, map: memmap2::Mmap)`.

2. In `persist.rs`, `mmap_subspace_verified(dir, manifest, w, b) ->
   Result<memmap2::Mmap>`: maps read-only via `mmap_subspace` (which checks
   `len == entry.size * 2`), then verifies the manifest checksum by hashing the
   mapped bytes directly (identical to `xxh3_of` over the decoded `u16`s, since the
   files are the native-endian `as_bytes` image; one streaming pass through the
   page cache).

3. `serve`: `load_db` always uses the mmap reader; `/api/meta` reports
   `"mmap": true`. Everything downstream (`analyze`, `play::best_*`, the opening
   search) is unchanged — they only see `Database`.

**Done when**: `cargo test --release` fully green (proves `Backing::Owned` changed
nothing); M1's test suite passes against an mmap-loaded dev database (`load_db` now
maps, so every server test exercises `insert_mmap`); with the full database, `serve`
binds within ~30s (the checksum pass), its resident set is clean file-backed pages
the OS can reclaim at will (no swap growth from the server's own footprint), and
analyses are still instant in the movement phase. Commit.

---

## Pitfall index (check against this before debugging anything for long)

| Symptom | Likely cause |
|---|---|
| Values look inverted for Black / only when playing Black | a second, stray color↔mover conversion — audit that `to_internal`/`to_state_json` are the only ones and that hands were swapped for `PlacementState` (M2.2) |
| Win/loss correct but depths off by one | mixing the two mappings: successor codes need the `+1` + parity flip (1.5); the current position's own code needs neither |
| "no entry found for key" panic | lookup into a missing subspace — the M1.4 guard is incomplete, or a placement analysis slipped past the M2.1 partial check |
| Move with capture shows the wrong `from` | diffing against `succ.white()` instead of `succ.black()` — successors are mover-flipped |
| Everything works except one specific mill capture | you are reimplementing a rule in JS instead of filtering `analysis.moves` |
| First opening analysis takes forever every time | transposition table not shared across requests (must live in `serve`'s loop, passed `&mut` into `analyze`) |
| Wasted-slot `0xFFFF` reads | not possible through this design — `lookup_pos` canonicalizes; if you see one, you bypassed `Database::lookup_pos` |
