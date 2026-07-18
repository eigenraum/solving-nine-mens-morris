//! HTTP server for the browser UI (see `ui-design.md`): one embedded static
//! page and a stateless JSON analysis endpoint over the solved database.
//!
//! The wire format always uses *physical colors* (white/black + whose
//! turn); the internal mover/opponent `Position` convention (see
//! `pos.rs`) is only ever touched inside [`to_internal`] and
//! [`to_state_json`] — every other function in this module works
//! entirely in one convention or the other, never both at once. See
//! `ui-implementation.md`'s pitfall index if you're debugging a values-
//! look-inverted-for-Black kind of bug: it is almost certainly a second,
//! stray conversion introduced somewhere outside those two functions.

use crate::board;
use crate::movegen;
use crate::opening::{self, PlacementState};
use crate::opening_cache;
use crate::persist::{self, Manifest};
use crate::play;
use crate::pos::{bits, Position};
use crate::retro::{self, Database};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const INDEX_HTML: &str = include_str!("../ui/index.html");

/// The opening-search transposition table. Must live for the lifetime of
/// the server process (see `ui-design.md` §6): every analysis warms every
/// later one, which is what makes placement-phase analyses fast after the
/// first probe of any given subtree. Shared across the worker threads
/// behind a `Mutex`, locked once per placement analysis (see [`analyze`]).
pub type Tt = crate::opening::Tt;

pub struct ServeOptions {
    pub bind: String,
    /// Load whatever subspaces exist instead of requiring all 49.
    /// Placement-phase analysis is refused; movement-phase analysis works
    /// for material pairs that are present. Development aid.
    pub allow_partial: bool,
    /// Run the empty-board opening solve at startup, into the server's
    /// long-lived `Tt`, so the first placement analysis is instant too.
    /// Runs *after* the socket is bound: the page and movement-phase
    /// analyses are served while it runs; placement analyses queue on
    /// the `Tt` lock until it finishes.
    pub warm: bool,
    /// Serve `index.html` from this directory instead of the copy
    /// embedded at compile time (edit-reload development loop).
    pub ui_dir: Option<PathBuf>,
}

pub struct Loaded {
    pub db: Database,
    /// True iff all 49 `(w,b)` subspaces for `w,b` in `3..=9` are loaded.
    pub complete: bool,
    pub subspaces: usize,
    pub manifest: Manifest,
}

/// Load the database by memory-mapping every subspace read-only,
/// verifying each checksum with one streaming pass through the map
/// (ui-implementation.md M6). The full database is on the order of a
/// typical machine's total RAM, so owned `Vec`s here would push the
/// whole system into swap; mapped, the OS keeps only the touched pages
/// resident and can drop them again under pressure. Differs from
/// `play`'s preamble only in what happens when subspaces are missing:
/// `allow_partial` turns a hard error into `complete: false`.
pub fn load_db(dir: &Path, allow_partial: bool) -> Result<Loaded> {
    let manifest = Manifest::load(dir)?;
    let missing: Vec<(usize, usize)> = (3..=9)
        .flat_map(|w| (3..=9).map(move |b| (w, b)))
        .filter(|&(w, b)| manifest.find(w, b).is_none())
        .collect();
    let complete = missing.is_empty();
    if !complete && !allow_partial {
        anyhow::bail!(
            "database at {} is incomplete ({} of 49 subspaces missing, e.g. {:?}) -- pass \
             --allow-partial for movement-phase-only analysis against a partial database, or \
             finish `ninemm solve --dir {}` first",
            dir.display(),
            missing.len(),
            &missing[..missing.len().min(5)],
            dir.display()
        );
    }
    let mut db = Database::new();
    for e in &manifest.entries {
        let mmap = persist::mmap_subspace_verified(dir, &manifest, e.w as usize, e.b as usize)
            .with_context(|| format!("mapping subspace ({},{})", e.w, e.b))?;
        db.insert_mmap(e.w as usize, e.b as usize, mmap);
    }
    let subspaces = manifest.entries.len();
    Ok(Loaded { db, complete, subspaces, manifest })
}

pub fn serve(dir: &Path, opts: &ServeOptions) -> Result<()> {
    let loaded = load_db(dir, opts.allow_partial)?;
    println!(
        "Mapped {} subspace(s) ({}), checksums verified.",
        loaded.subspaces,
        if loaded.complete {
            "complete"
        } else {
            "partial"
        }
    );

    let tt: Mutex<Tt> = Mutex::new(opening_cache::load_or_empty(dir, &loaded.manifest));

    // Bind before the (potentially long) warm solve so the browser sees
    // a listening server immediately instead of connection-refused.
    let server = tiny_http::Server::http(&opts.bind)
        .map_err(|e| anyhow::anyhow!("failed to bind {}: {e}", opts.bind))?;
    println!("Serving on http://{}", opts.bind);

    // A small worker pool (ui-design.md "Threading"): a placement
    // analysis can run for seconds to minutes, and with a single loop it
    // would block even `GET /` and `/api/meta`, making the UI look dead.
    // Placement analyses still serialize on the `Tt` lock; everything
    // else stays responsive on the other workers.
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                while let Ok(mut req) = server.recv() {
                    // A panicking analysis must cost one 500 response,
                    // not silently shrink the worker pool.
                    let response =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            route(&mut req, &loaded, opts, &tt)
                        }))
                        .unwrap_or_else(|_| {
                            error_response(500, "internal error: analysis panicked (see server log)".to_string())
                        });
                    let _ = req.respond(response); // client hung up: ignore, keep serving
                }
            });
        }
        if opts.warm {
            if loaded.complete {
                print!("Warming opening transposition table (empty-board solve)... ");
                std::io::stdout().flush().ok();
                let start = std::time::Instant::now();
                let mut tt = tt.lock().unwrap_or_else(|p| p.into_inner());
                let v = opening::solve(&PlacementState::initial(), &loaded.db, &mut tt);
                println!(
                    "done in {:.1}s: root value = {v:?}",
                    start.elapsed().as_secs_f64()
                );
            } else {
                println!("--warm skipped: database is partial, placement analysis is unavailable.");
            }
        }
    });
    Ok(())
}

fn json_response(status: u16, body: String) -> tiny_http::Response<Cursor<Vec<u8>>> {
    let header = tiny_http::Header::from_bytes(
        &b"Content-Type"[..],
        &b"application/json; charset=utf-8"[..],
    )
    .expect("static header is valid");
    tiny_http::Response::from_string(body)
        .with_status_code(status)
        .with_header(header)
}

fn html_response(status: u16, body: String) -> tiny_http::Response<Cursor<Vec<u8>>> {
    let header =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .expect("static header is valid");
    tiny_http::Response::from_string(body)
        .with_status_code(status)
        .with_header(header)
}

fn error_response(status: u16, message: String) -> tiny_http::Response<Cursor<Vec<u8>>> {
    json_response(status, serde_json::json!({ "error": message }).to_string())
}

fn route(
    req: &mut tiny_http::Request,
    loaded: &Loaded,
    opts: &ServeOptions,
    tt: &Mutex<Tt>,
) -> tiny_http::Response<Cursor<Vec<u8>>> {
    let url = req.url().to_string();
    let is_get = matches!(req.method(), tiny_http::Method::Get);
    let is_post = matches!(req.method(), tiny_http::Method::Post);

    if is_get && url == "/" {
        let html = match &opts.ui_dir {
            Some(dir) => std::fs::read_to_string(dir.join("index.html")).unwrap_or_else(|e| {
                format!("failed to read {}: {e}", dir.join("index.html").display())
            }),
            None => INDEX_HTML.to_string(),
        };
        return html_response(200, html);
    }
    if is_get && url == "/api/meta" {
        let body = serde_json::json!({
            "complete": loaded.complete,
            "subspaces": loaded.subspaces,
            "mmap": true,
        })
        .to_string();
        return json_response(200, body);
    }
    if is_post && url == "/api/analyze" {
        let mut body = String::new();
        if let Err(e) = req.as_reader().read_to_string(&mut body) {
            return error_response(400, format!("failed to read request body: {e}"));
        }
        let parsed: Result<AnalyzeRequest, _> = serde_json::from_str(&body);
        return match parsed {
            Ok(analyze_req) => match analyze(&analyze_req, &loaded.db, loaded.complete, tt) {
                Ok(resp) => json_response(
                    200,
                    serde_json::to_string(&resp).expect("response always serializes"),
                ),
                Err(e) => error_response(e.status, e.message),
            },
            Err(e) => error_response(400, format!("invalid request body: {e}")),
        };
    }
    error_response(404, "not found".to_string())
}

// ---------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Color {
    White,
    Black,
}

impl Color {
    fn other(self) -> Color {
        match self {
            Color::White => Color::Black,
            Color::Black => Color::White,
        }
    }
}

/// A game state in physical-color terms — the only shape that crosses the
/// wire. Point indices are `0..24` per `readme-database.md` §1.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StateJson {
    pub white: Vec<u8>,
    pub black: Vec<u8>,
    pub turn: Color,
    #[serde(default)]
    pub white_hand: u8,
    #[serde(default)]
    pub black_hand: u8,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeRequest {
    #[serde(flatten)]
    pub state: StateJson,
    #[serde(default)]
    pub evaluate: bool,
    /// Compute `engineMove` in the *placement* phase even when `evaluate`
    /// is false. Off by default because it costs a full opening search
    /// (the early-exit-on-first-win never fires from drawn states, e.g.
    /// the empty board), which a client showing a human-owned turn would
    /// pay for and then ignore. Movement-phase `engineMove` is always
    /// computed — it really is cheap there (a handful of probes) — and
    /// `evaluate: true` placement analyses derive it from the per-move
    /// values at no extra cost, so this flag only matters for
    /// `evaluate: false` placement requests where the engine owns the turn.
    #[serde(default)]
    pub engine: bool,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Win,
    Draw,
    Loss,
}

#[derive(Serialize, Clone, Copy, Debug)]
pub struct ValueJson {
    pub outcome: Outcome,
    pub plies: Option<u16>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct MoveJson {
    pub from: Option<u8>,
    pub to: u8,
    pub capture: Option<u8>,
    pub notation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ValueJson>,
    pub result: StateJson,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ResultJson {
    pub winner: Color,
    pub reason: &'static str, // "fewerThanThree" | "noMoves"
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResponse {
    pub phase: &'static str, // "placement" | "movement"
    pub result: Option<ResultJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ValueJson>,
    pub moves: Vec<MoveJson>,
    pub engine_move: Option<usize>,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

fn bad(message: impl Into<String>) -> ApiError {
    ApiError {
        status: 400,
        message: message.into(),
    }
}

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError {
        status: 409,
        message: message.into(),
    }
}

// ---------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------

fn validate(state: &StateJson) -> Result<(u32, u32), ApiError> {
    let mut white_bits = 0u32;
    for &p in &state.white {
        if p >= 24 {
            return Err(bad(format!("point index {p} out of range 0..24")));
        }
        if white_bits & (1 << p) != 0 {
            return Err(bad(format!("duplicate white point {p}")));
        }
        white_bits |= 1 << p;
    }
    let mut black_bits = 0u32;
    for &p in &state.black {
        if p >= 24 {
            return Err(bad(format!("point index {p} out of range 0..24")));
        }
        if black_bits & (1 << p) != 0 {
            return Err(bad(format!("duplicate black point {p}")));
        }
        black_bits |= 1 << p;
    }
    if white_bits & black_bits != 0 {
        return Err(bad("white and black occupy the same point"));
    }
    if state.white_hand > 9 || state.black_hand > 9 {
        return Err(bad("hand count exceeds 9"));
    }
    if state.white.len() + state.white_hand as usize > 9 {
        return Err(bad("white has more than 9 total stones (board + hand)"));
    }
    if state.black.len() + state.black_hand as usize > 9 {
        return Err(bad("black has more than 9 total stones (board + hand)"));
    }
    if state.white_hand > 0 || state.black_hand > 0 {
        let ok = match state.turn {
            Color::White => state.white_hand == state.black_hand,
            Color::Black => state.black_hand == state.white_hand + 1,
        };
        if !ok {
            return Err(bad(
                "hand counts are inconsistent with strict placement alternation",
            ));
        }
    }
    Ok((white_bits, black_bits))
}

// ---------------------------------------------------------------------
// The one perspective conversion point (both directions)
// ---------------------------------------------------------------------

/// Physical colors -> internal mover/opponent `Position`.
fn to_internal(white_bits: u32, black_bits: u32, turn: Color) -> Position {
    let p = Position::new(white_bits, black_bits);
    match turn {
        Color::White => p,
        Color::Black => p.swap_colors(),
    }
}

/// Internal mover/opponent `Position` whose mover is the player `turn` ->
/// physical-color wire state.
fn to_state_json(pos: Position, turn: Color, white_hand: u8, black_hand: u8) -> StateJson {
    let phys = match turn {
        Color::White => pos,
        Color::Black => pos.swap_colors(),
    };
    StateJson {
        white: bits(phys.white()).map(|p| p as u8).collect(),
        black: bits(phys.black()).map(|p| p as u8).collect(),
        turn,
        white_hand,
        black_hand,
    }
}

fn game_over_response(phase: &'static str, winner: Color, reason: &'static str) -> AnalyzeResponse {
    AnalyzeResponse {
        phase,
        result: Some(ResultJson { winner, reason }),
        value: None,
        moves: Vec::new(),
        engine_move: None,
    }
}

// ---------------------------------------------------------------------
// Analysis (pure function: no HTTP types, directly unit-testable)
// ---------------------------------------------------------------------

pub fn analyze(
    req: &AnalyzeRequest,
    db: &Database,
    complete: bool,
    tt: &Mutex<Tt>,
) -> Result<AnalyzeResponse, ApiError> {
    let (white_bits, black_bits) = validate(&req.state)?;
    let turn = req.state.turn;
    let is_placement = req.state.white_hand > 0 || req.state.black_hand > 0;
    if is_placement {
        analyze_placement(req, white_bits, black_bits, turn, db, complete, tt)
    } else {
        analyze_movement(req, white_bits, black_bits, turn, db, complete)
    }
}

fn movement_notation(from: u8, to: u8, capture: Option<u8>) -> String {
    let mut s = format!(
        "{}-{}",
        board::point_name(from as usize),
        board::point_name(to as usize)
    );
    if let Some(c) = capture {
        s.push('x');
        s.push_str(&board::point_name(c as usize));
    }
    s
}

fn placement_notation(to: u8, capture: Option<u8>) -> String {
    let mut s = board::point_name(to as usize);
    if let Some(c) = capture {
        s.push('x');
        s.push_str(&board::point_name(c as usize));
    }
    s
}

/// Value of making a move, for the player making it, given the successor
/// position (which is from the *next* mover's perspective) -- the same
/// rule `play::best_movement_move` applies internally
/// (`readme-database.md` §5): the successor's own stored code belongs to
/// its mover, so it is negated (parity flip) and shifted by one ply to
/// become "our" value for having made the move.
fn movement_move_value(succ: Position, db: &Database) -> ValueJson {
    let code = if succ.white_count() < 3 {
        0
    } else {
        db.lookup_pos(succ)
    };
    if code == retro::DRAW {
        ValueJson {
            outcome: Outcome::Draw,
            plies: None,
        }
    } else if code.is_multiple_of(2) {
        ValueJson {
            outcome: Outcome::Win,
            plies: Some(code + 1),
        } // their loss in c -> our win in c+1
    } else {
        ValueJson {
            outcome: Outcome::Loss,
            plies: Some(code + 1),
        } // their win in c -> our loss in c+1
    }
}

/// Value of the current position, for the side to move -- a direct
/// lookup, no perspective shift (the stored code already belongs to this
/// mover).
fn movement_position_value(pos: Position, db: &Database) -> ValueJson {
    let code = db.lookup_pos(pos);
    if code == retro::DRAW {
        ValueJson {
            outcome: Outcome::Draw,
            plies: None,
        }
    } else if code.is_multiple_of(2) {
        ValueJson {
            outcome: Outcome::Loss,
            plies: Some(code),
        }
    } else {
        ValueJson {
            outcome: Outcome::Win,
            plies: Some(code),
        }
    }
}

fn analyze_movement(
    req: &AnalyzeRequest,
    white_bits: u32,
    black_bits: u32,
    turn: Color,
    db: &Database,
    complete: bool,
) -> Result<AnalyzeResponse, ApiError> {
    let pos = to_internal(white_bits, black_bits, turn);

    if pos.white_count() < 3 {
        return Ok(game_over_response(
            "movement",
            turn.other(),
            "fewerThanThree",
        ));
    }
    if pos.black_count() < 3 {
        return Ok(game_over_response("movement", turn, "fewerThanThree"));
    }
    let succs = movegen::successors(pos);
    if succs.is_empty() {
        return Ok(game_over_response("movement", turn.other(), "noMoves"));
    }

    if !complete {
        let w = pos.white_count() as usize;
        let b = pos.black_count() as usize;
        let mut needed = vec![(w, b), (b, w)];
        if b > 3 {
            needed.push((b - 1, w));
        }
        for (nw, nb) in needed {
            if !db.has(nw, nb) {
                return Err(conflict(format!(
                    "subspace ({nw},{nb}) is not loaded (partial database) -- this analysis needs it"
                )));
            }
        }
    }

    let mut moves = Vec::with_capacity(succs.len());
    for &succ in &succs {
        // succ is mover-flipped: succ.black() is the current mover's
        // stones after the move (incl. the moved stone); succ.white() is
        // the opponent's stones, post-capture.
        let from_bits = pos.white() & !succ.black();
        let to_bits = succ.black() & !pos.white();
        let cap_bits = pos.black() & !succ.white();
        let from = from_bits.trailing_zeros() as u8;
        let to = to_bits.trailing_zeros() as u8;
        let capture = (cap_bits != 0).then(|| cap_bits.trailing_zeros() as u8);

        let value = req.evaluate.then(|| movement_move_value(succ, db));
        moves.push(MoveJson {
            from: Some(from),
            to,
            capture,
            notation: movement_notation(from, to, capture),
            value,
            result: to_state_json(succ, turn.other(), 0, 0),
        });
    }

    let value = req.evaluate.then(|| movement_position_value(pos, db));

    let engine_move = play::best_movement_move(pos, db).map(|c| {
        succs
            .iter()
            .position(|s| *s == c.successor)
            .expect("choice comes from succs")
    });

    Ok(AnalyzeResponse {
        phase: "movement",
        result: None,
        value,
        moves,
        engine_move,
    })
}

fn analyze_placement(
    req: &AnalyzeRequest,
    white_bits: u32,
    black_bits: u32,
    turn: Color,
    db: &Database,
    complete: bool,
    tt: &Mutex<Tt>,
) -> Result<AnalyzeResponse, ApiError> {
    if !complete {
        return Err(conflict(
            "placement analysis requires the complete 49-subspace database (the opening search \
             can reach nearly any material split)",
        ));
    }

    let pos = to_internal(white_bits, black_bits, turn);
    let (mover_hand, opp_hand) = match turn {
        Color::White => (req.state.white_hand, req.state.black_hand),
        Color::Black => (req.state.black_hand, req.state.white_hand),
    };
    let ps = PlacementState {
        pos,
        mover_hand,
        opp_hand,
    };

    if ps.total_mover() < 3 {
        return Ok(game_over_response(
            "placement",
            turn.other(),
            "fewerThanThree",
        ));
    }
    if ps.total_opp() < 3 {
        return Ok(game_over_response("placement", turn, "fewerThanThree"));
    }

    let succs = opening::successors(&ps);
    // Can't actually happen: a player with stones in hand always has an
    // empty point to place on (at most 18 of 24 points are ever
    // occupied). Kept as an explicit terminal rather than silently
    // mispropagating, matching opening::negamax's own defensive terminal.
    if succs.is_empty() {
        return Ok(game_over_response("placement", turn.other(), "noMoves"));
    }

    // One placement *search* at a time: the whole analysis holds the
    // shared TT so its own successor solves warm each other. Requests
    // that only enumerate moves (no `evaluate`, no `engine`) never touch
    // the TT and never wait here — the board stays instant even while
    // another analysis (or the --warm solve) holds the lock. Poison
    // recovery is sound because a panicked search leaves only complete,
    // individually-valid TT entries behind.
    let mut tt_guard = (req.evaluate || req.engine)
        .then(|| tt.lock().unwrap_or_else(|p| p.into_inner()));

    let next = turn.other();
    let mut moves = Vec::with_capacity(succs.len());
    let mut values: Vec<Option<ValueJson>> = Vec::with_capacity(succs.len());
    for s in &succs {
        let to_bits = s.pos.black() & !ps.pos.white();
        let cap_bits = ps.pos.black() & !s.pos.white();
        let to = to_bits.trailing_zeros() as u8;
        let capture = (cap_bits != 0).then(|| cap_bits.trailing_zeros() as u8);
        let (wh, bh) = match next {
            Color::White => (s.mover_hand, s.opp_hand),
            Color::Black => (s.opp_hand, s.mover_hand),
        };

        let value = req.evaluate.then(|| {
            // opening::solve gives the value for s's mover (the *next*
            // player); negate it to get the value of making this move for
            // the current mover.
            let v = opening::solve(s, db, tt_guard.as_mut().expect("evaluate implies the TT lock is held"));
            let outcome = match v {
                opening::Value::Win => Outcome::Loss,
                opening::Value::Draw => Outcome::Draw,
                opening::Value::Loss => Outcome::Win,
            };
            ValueJson {
                outcome,
                plies: None,
            }
        });
        values.push(value);

        moves.push(MoveJson {
            from: None,
            to,
            capture,
            notation: placement_notation(to, capture),
            value,
            result: to_state_json(s.pos, next, wh, bh),
        });
    }

    let value = req.evaluate.then(|| {
        let v = opening::solve(&ps, db, tt_guard.as_mut().expect("evaluate implies the TT lock is held"));
        let outcome = match v {
            opening::Value::Win => Outcome::Win,
            opening::Value::Draw => Outcome::Draw,
            opening::Value::Loss => Outcome::Loss,
        };
        ValueJson {
            outcome,
            plies: None,
        }
    });

    let engine_move = if req.evaluate {
        // Reuse the values already computed above; no need to search again.
        values
            .iter()
            .position(|v| {
                matches!(
                    v,
                    Some(ValueJson {
                        outcome: Outcome::Win,
                        ..
                    })
                )
            })
            .or_else(|| {
                values.iter().position(|v| {
                    matches!(
                        v,
                        Some(ValueJson {
                            outcome: Outcome::Draw,
                            ..
                        })
                    )
                })
            })
            .or(Some(0))
    } else if req.engine {
        let tt = tt_guard.as_mut().expect("engine implies the TT lock is held");
        play::best_placement_move(&ps, db, tt).map(|choice| {
            succs
                .iter()
                .position(|s| *s == choice)
                .expect("choice comes from succs")
        })
    } else {
        // Neither requested: don't pay for an opening search the client
        // will not display (e.g. the initial page load on a human turn).
        None
    };

    Ok(AnalyzeResponse {
        phase: "placement",
        result: None,
        value,
        moves,
        engine_move,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{self, SubspaceId};
    use crate::orchestrate;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn build_dev_db(tag: &str) -> Loaded {
        let tmp = std::env::temp_dir().join(format!(
            "ninemm_server_test_{tag}_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&tmp).ok();
        orchestrate::solve_all(&tmp, Some(7)).unwrap(); // {3,3} and {3,4}/{4,3}
        let loaded = load_db(&tmp, true).unwrap();
        std::fs::remove_dir_all(&tmp).ok();
        loaded
    }

    fn req(state: StateJson, evaluate: bool) -> AnalyzeRequest {
        AnalyzeRequest { state, evaluate, engine: false }
    }

    #[test]
    fn rejects_duplicate_point() {
        let loaded = build_dev_db("dup");
        let tt = Mutex::new(Tt::new());
        let s = StateJson {
            white: vec![0, 0],
            black: vec![1, 2, 3],
            turn: Color::White,
            white_hand: 0,
            black_hand: 0,
        };
        let err = analyze(&req(s, false), &loaded.db, loaded.complete, &tt).unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[test]
    fn rejects_overlapping_colors() {
        let loaded = build_dev_db("overlap");
        let tt = Mutex::new(Tt::new());
        let s = StateJson {
            white: vec![0, 1, 2],
            black: vec![2, 3, 4],
            turn: Color::White,
            white_hand: 0,
            black_hand: 0,
        };
        let err = analyze(&req(s, false), &loaded.db, loaded.complete, &tt).unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[test]
    fn rejects_too_many_total_stones() {
        let loaded = build_dev_db("toomany");
        let tt = Mutex::new(Tt::new());
        let s = StateJson {
            white: vec![0, 1, 2, 3, 4, 5, 6, 7, 8],
            black: vec![9, 10, 11],
            turn: Color::White,
            white_hand: 1,
            black_hand: 0,
        };
        let err = analyze(&req(s, false), &loaded.db, loaded.complete, &tt).unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[test]
    fn rejects_bad_hand_alternation() {
        let loaded = build_dev_db("alt");
        let tt = Mutex::new(Tt::new());
        let s = StateJson {
            white: vec![],
            black: vec![],
            turn: Color::White,
            white_hand: 9,
            black_hand: 8,
        };
        let err = analyze(&req(s, false), &loaded.db, loaded.complete, &tt).unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[test]
    fn fewer_than_three_is_game_over() {
        let loaded = build_dev_db("fewer");
        let tt = Mutex::new(Tt::new());
        let s = StateJson {
            white: vec![0, 1],
            black: vec![9, 10, 11],
            turn: Color::White,
            white_hand: 0,
            black_hand: 0,
        };
        let resp = analyze(&req(s, false), &loaded.db, loaded.complete, &tt).unwrap();
        assert!(resp.moves.is_empty());
        let result = resp.result.unwrap();
        assert_eq!(result.winner, Color::Black);
        assert_eq!(result.reason, "fewerThanThree");
    }

    #[test]
    fn blocked_position_is_no_moves_game_over() {
        let loaded = build_dev_db("blocked");
        let tt = Mutex::new(Tt::new());
        // Find a blocked position in the {4,3} subspace by scanning canonical slots.
        let sub = SubspaceId::new(4, 3);
        let mut found = None;
        for idx in 0..index::subspace_size(sub) {
            if !index::is_canonical_slot(sub, idx) {
                continue;
            }
            let pos = index::unindex(sub, idx);
            if pos.is_blocked() {
                found = Some(pos);
                break;
            }
        }
        let Some(pos) = found else {
            // Not finding one is not itself a bug in this test; skip.
            return;
        };
        let s = StateJson {
            white: bits(pos.white()).map(|p| p as u8).collect(),
            black: bits(pos.black()).map(|p| p as u8).collect(),
            turn: Color::White,
            white_hand: 0,
            black_hand: 0,
        };
        let resp = analyze(&req(s, false), &loaded.db, loaded.complete, &tt).unwrap();
        assert!(resp.moves.is_empty());
        let result = resp.result.unwrap();
        assert_eq!(result.winner, Color::Black);
        assert_eq!(result.reason, "noMoves");
    }

    #[test]
    fn partial_database_rejects_out_of_range_movement_analysis() {
        let loaded = build_dev_db("partial");
        assert!(!loaded.complete);
        let tt = Mutex::new(Tt::new());
        // 5-vs-5 is outside the {3,3}/{4,3} dev database.
        let s = StateJson {
            white: vec![0, 1, 2, 3, 4],
            black: vec![9, 10, 11, 12, 13],
            turn: Color::White,
            white_hand: 0,
            black_hand: 0,
        };
        let err = analyze(&req(s, true), &loaded.db, loaded.complete, &tt).unwrap_err();
        assert_eq!(err.status, 409);
    }

    #[test]
    fn partial_database_rejects_placement_analysis() {
        let loaded = build_dev_db("partial_placement");
        let tt = Mutex::new(Tt::new());
        let s = StateJson {
            white: vec![],
            black: vec![],
            turn: Color::White,
            white_hand: 9,
            black_hand: 9,
        };
        let err = analyze(&req(s, true), &loaded.db, loaded.complete, &tt).unwrap_err();
        assert_eq!(err.status, 409);
    }

    /// Color-swap invariance: analyzing the same abstract position from
    /// White's turn with (white=A, black=B) and from Black's turn with
    /// (white=B, black=A) must produce identical analyses modulo
    /// relabeling. This is the test that catches a perspective slip.
    #[test]
    fn color_swap_invariance() {
        let loaded = build_dev_db("swap");
        let sub = SubspaceId::new(3, 3);
        let size = index::subspace_size(sub);
        let step = (size / 300).max(1);
        let mut checked = 0;
        let mut idx = 0u64;
        while idx < size {
            idx += step;
            if !index::is_canonical_slot(sub, idx.min(size - 1)) {
                continue;
            }
            let pos = index::unindex(sub, idx.min(size - 1));
            let a = bits(pos.white()).map(|p| p as u8).collect::<Vec<_>>();
            let b = bits(pos.black()).map(|p| p as u8).collect::<Vec<_>>();

            let tt1 = Mutex::new(Tt::new());
            let s1 = StateJson {
                white: a.clone(),
                black: b.clone(),
                turn: Color::White,
                white_hand: 0,
                black_hand: 0,
            };
            let r1 = analyze(&req(s1, true), &loaded.db, loaded.complete, &tt1).unwrap();

            let tt2 = Mutex::new(Tt::new());
            let s2 = StateJson {
                white: b.clone(),
                black: a.clone(),
                turn: Color::Black,
                white_hand: 0,
                black_hand: 0,
            };
            let r2 = analyze(&req(s2, true), &loaded.db, loaded.complete, &tt2).unwrap();

            assert_eq!(r1.phase, r2.phase);
            assert_eq!(
                r1.value.map(|v| (v.outcome, v.plies)),
                r2.value.map(|v| (v.outcome, v.plies)),
                "position value mismatch under color swap"
            );
            assert_eq!(r1.moves.len(), r2.moves.len());

            let mut m1: Vec<_> = r1
                .moves
                .iter()
                .map(|m| {
                    (
                        m.from,
                        m.to,
                        m.capture,
                        m.value.map(|v| (v.outcome, v.plies)),
                    )
                })
                .collect();
            let mut m2: Vec<_> = r2
                .moves
                .iter()
                .map(|m| {
                    (
                        m.from,
                        m.to,
                        m.capture,
                        m.value.map(|v| (v.outcome, v.plies)),
                    )
                })
                .collect();
            m1.sort();
            m2.sort();
            assert_eq!(m1, m2, "move set mismatch under color swap");
            checked += 1;
        }
        assert!(
            checked > 20,
            "expected to check a reasonable number of positions, got {checked}"
        );
    }

    /// Self-consistency (the depth test, design.md §8 / play.rs's own soak
    /// test, applied at the API layer): for every reported move value
    /// "win in d", the successor's own position value must be "loss in
    /// d-1" (or a fewer-than-three-stones game over when d==1); draws map
    /// to draws; losses map symmetrically.
    #[test]
    fn move_value_matches_successor_position_value() {
        let loaded = build_dev_db("selfconsistent");
        let sub = SubspaceId::new(4, 3);
        let size = index::subspace_size(sub);
        let step = (size / 200).max(1);
        let mut checked = 0;
        let mut idx = 0u64;
        while idx < size {
            idx += step;
            let i = idx.min(size - 1);
            if !index::is_canonical_slot(sub, i) {
                continue;
            }
            let pos = index::unindex(sub, i);
            let s = StateJson {
                white: bits(pos.white()).map(|p| p as u8).collect(),
                black: bits(pos.black()).map(|p| p as u8).collect(),
                turn: Color::White,
                white_hand: 0,
                black_hand: 0,
            };
            let tt = Mutex::new(Tt::new());
            let resp = analyze(&req(s, true), &loaded.db, loaded.complete, &tt).unwrap();
            if resp.result.is_some() {
                continue;
            }
            for m in &resp.moves {
                let Some(v) = m.value else { continue };
                let tt2 = Mutex::new(Tt::new());
                let succ_resp = analyze(
                    &req(m.result.clone(), true),
                    &loaded.db,
                    loaded.complete,
                    &tt2,
                )
                .unwrap();
                match v.outcome {
                    Outcome::Draw => {
                        if let Some(sv) = succ_resp.value {
                            assert_eq!(sv.outcome, Outcome::Draw);
                        } else {
                            panic!("expected successor position value for a draw move");
                        }
                    }
                    Outcome::Win => {
                        let d = v.plies.unwrap();
                        if d == 1 {
                            let r = succ_resp
                                .result
                                .expect("win in 1 must capture the opponent below 3 stones");
                            assert_eq!(r.reason, "fewerThanThree");
                        } else {
                            let sv = succ_resp
                                .value
                                .expect("non-terminal successor must have a value");
                            assert_eq!(sv.outcome, Outcome::Loss);
                            assert_eq!(sv.plies, Some(d - 1));
                        }
                    }
                    Outcome::Loss => {
                        let d = v.plies.unwrap();
                        let sv = succ_resp
                            .value
                            .expect("non-terminal successor must have a value");
                        assert_eq!(sv.outcome, Outcome::Win);
                        assert_eq!(sv.plies, Some(d - 1));
                    }
                }
            }
            checked += 1;
        }
        assert!(
            checked > 20,
            "expected to check a reasonable number of positions, got {checked}"
        );
    }

    #[test]
    fn engine_move_index_is_valid() {
        let loaded = build_dev_db("engine");
        let tt = Mutex::new(Tt::new());
        let sub = SubspaceId::new(3, 3);
        let pos = index::unindex(sub, 0);
        let s = StateJson {
            white: bits(pos.white()).map(|p| p as u8).collect(),
            black: bits(pos.black()).map(|p| p as u8).collect(),
            turn: Color::White,
            white_hand: 0,
            black_hand: 0,
        };
        let resp = analyze(&req(s, false), &loaded.db, loaded.complete, &tt).unwrap();
        if let Some(i) = resp.engine_move {
            assert!(i < resp.moves.len());
        }
    }
}
