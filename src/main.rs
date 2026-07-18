use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ninemm", about = "Solve Nine Men's Morris")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print board geometry (debug aid).
    Board,
    /// Print subspace sizes and canonical white-set table sizes.
    Stats,
    /// Solve the mid/endgame database, pair by pair, bottom-up. Resumable:
    /// re-running skips pairs already solved on disk with a matching
    /// checksum.
    Solve {
        /// Directory to read/write the database files and manifest.
        #[arg(long, default_value = "db")]
        dir: PathBuf,
        /// Only solve pairs whose combined stone count is at or below this
        /// (useful for partial runs / benchmarking). Omit to solve
        /// everything (all 28 pairs, up to {9,9}).
        #[arg(long)]
        max_total: Option<usize>,
    },
    /// Forward-consistency scan (design.md §8.1): for every solved pair,
    /// recompute each state's value directly from its successors' stored
    /// values (independent of the retrograde solver's own logic) and
    /// report any mismatches. Stops at the first pair with an
    /// inconsistency, since later pairs depend on it.
    Verify {
        #[arg(long, default_value = "db")]
        dir: PathBuf,
    },
    /// Win/loss/draw tallies and deepest win/loss per solved subspace.
    DbStats {
        #[arg(long, default_value = "db")]
        dir: PathBuf,
    },
    /// Play an interactive game against the perfect-play engine. Moves are
    /// entered in "a1".."g7" notation: a placement or a "from to" pair
    /// (e.g. "a1 a4") once past the opening.
    Play {
        #[arg(long, default_value = "db")]
        dir: PathBuf,
        /// Which side the human plays.
        #[arg(long, default_value = "white")]
        human: String,
    },
    /// Serve the browser UI and JSON analysis API over the solved
    /// database (see ui-design.md).
    Serve {
        #[arg(long, default_value = "db")]
        dir: PathBuf,
        /// Address to bind. This is a local analysis tool -- keep it on
        /// localhost unless you understand the exposure.
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
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Board => {
            for p in 0..ninemm::board::N {
                println!(
                    "{:2} {} adj={:024b} mills={:?}",
                    p,
                    ninemm::board::point_name(p),
                    ninemm::board::ADJ[p],
                    ninemm::board::POINT_MILLS[p]
                );
            }
        }
        Commands::Stats => {
            use ninemm::index::{self, SubspaceId};
            for w in 3..=9usize {
                println!("w={w} canonical_white_sets={}", index::n_canonical_white(w));
            }
            let mut total = 0u64;
            for sub in index::all_subspaces() {
                let sz = index::subspace_size(sub);
                total += sz;
                if sub.w >= 8
                    || (sub.w, sub.b) == (SubspaceId::new(3, 3).w, SubspaceId::new(3, 3).b)
                {
                    println!("subspace {}-{}: {sz}", sub.w, sub.b);
                }
            }
            println!("total slots across all 49 subspaces: {total}");
            println!(
                "solve order (28 unordered pairs): {:?}",
                index::solve_order()
            );
        }
        Commands::Solve { dir, max_total } => {
            ninemm::orchestrate::solve_all(&dir, max_total)?;
        }
        Commands::Verify { dir } => {
            let reports = ninemm::verify::verify_all(&dir)?;
            let mut all_ok = true;
            for r in &reports {
                if r.ok() {
                    println!("[{}-{}] OK ({} states checked)", r.a, r.b, r.checked);
                } else {
                    all_ok = false;
                    println!(
                        "[{}-{}] FAILED: {} mismatches out of {} checked (showing up to 50)",
                        r.a,
                        r.b,
                        r.mismatches.len(),
                        r.checked
                    );
                    for m in &r.mismatches {
                        println!(
                            "    idx={} stored={} expected={}",
                            m.idx, m.stored, m.expected
                        );
                    }
                }
            }
            if !all_ok {
                anyhow::bail!("verification found inconsistencies");
            }
        }
        Commands::Play { dir, human } => {
            run_play(&dir, &human)?;
        }
        Commands::Serve {
            dir,
            bind,
            allow_partial,
            warm,
            ui_dir,
        } => {
            ninemm::server::serve(
                &dir,
                &ninemm::server::ServeOptions {
                    bind,
                    allow_partial,
                    warm,
                    ui_dir,
                },
            )?;
        }
        Commands::DbStats { dir } => {
            use ninemm::index::SubspaceId;
            use ninemm::persist::{self, Manifest};
            let manifest = Manifest::load(&dir)?;
            for entry in &manifest.entries {
                let sub = SubspaceId::new(entry.w as usize, entry.b as usize);
                let data = persist::read_subspace_verified(
                    &dir,
                    &manifest,
                    entry.w as usize,
                    entry.b as usize,
                )?;
                let t = ninemm::verify::tally(sub, &data);
                println!(
                    "{}-{}: wins={} losses={} draws={} max_win_depth={} max_loss_depth={}",
                    entry.w, entry.b, t.wins, t.losses, t.draws, t.max_win_depth, t.max_loss_depth
                );
            }
        }
    }
    Ok(())
}

/// Interactive text UI. Internally everything uses the "mover/opponent"
/// perspective (see pos.rs); `white_to_move` tracks whose *actual* turn
/// it is so we can map back to real White/Black for display and input.
fn run_play(dir: &std::path::Path, human: &str) -> anyhow::Result<()> {
    use ninemm::board;
    use ninemm::opening::{self, PlacementState};
    use ninemm::persist::{self, Manifest};
    use ninemm::play;
    use ninemm::pos::Position;
    use ninemm::retro::Database;
    use std::collections::HashMap;
    use std::io::{self, BufRead, Write};

    let human_is_white = match human.to_lowercase().as_str() {
        "white" | "w" => true,
        "black" | "b" => false,
        other => anyhow::bail!("--human must be 'white' or 'black', got '{other}'"),
    };

    println!("Loading database from {}...", dir.display());
    let manifest = Manifest::load(dir)?;
    if manifest.entries.is_empty() {
        anyhow::bail!(
            "no solved database found in {} -- run `ninemm solve` first",
            dir.display()
        );
    }
    let expected: Vec<(usize, usize)> =
        (3..=9).flat_map(|w| (3..=9).map(move |b| (w, b))).collect();
    let missing: Vec<(usize, usize)> = expected
        .iter()
        .copied()
        .filter(|&(w, b)| {
            !manifest
                .entries
                .iter()
                .any(|e| e.w as usize == w && e.b as usize == b)
        })
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "database at {} is incomplete ({} of 49 subspaces missing, e.g. {:?}) -- perfect play needs the full \
             solve (`ninemm solve --dir {}`) to finish first; a partial database will panic mid-search as soon as \
             it needs an unsolved subspace",
            dir.display(),
            missing.len(),
            &missing[..missing.len().min(5)],
            dir.display()
        );
    }
    let mut db = Database::new();
    for e in &manifest.entries {
        let data = persist::read_subspace_verified(dir, &manifest, e.w as usize, e.b as usize)?;
        db.insert(e.w as usize, e.b as usize, data);
    }
    println!(
        "Loaded {} subspaces. You are {}.",
        manifest.entries.len(),
        if human_is_white { "White" } else { "Black" }
    );

    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut tt: HashMap<(Position, u8, u8), i8> = HashMap::new();

    let mut placement: Option<PlacementState> = Some(PlacementState::initial());
    let mut movement: Option<Position> = None;
    let mut white_to_move = true;

    // Ask a numbered question, reading a 1-based index from stdin.
    let ask_choice =
        |lines: &mut dyn Iterator<Item = io::Result<String>>, n: usize| -> anyhow::Result<usize> {
            loop {
                print!("Choice (1-{n}): ");
                io::stdout().flush()?;
                let Some(line) = lines.next() else {
                    anyhow::bail!("input closed");
                };
                if let Ok(i) = line?.trim().parse::<usize>() {
                    if (1..=n).contains(&i) {
                        return Ok(i - 1);
                    }
                }
                println!("Enter a number between 1 and {n}.");
            }
        };

    loop {
        let real_pos = if let Some(ps) = &placement {
            if white_to_move {
                ps.pos
            } else {
                ps.pos.swap_colors()
            }
        } else {
            let pos = movement.unwrap();
            if white_to_move {
                pos
            } else {
                pos.swap_colors()
            }
        };

        println!();
        println!("{}", play::render(real_pos));
        if let Some(ps) = &placement {
            let (wh, bh) = if white_to_move {
                (ps.mover_hand, ps.opp_hand)
            } else {
                (ps.opp_hand, ps.mover_hand)
            };
            println!("(in hand: White={wh} Black={bh})");
        }
        let mover_label = if white_to_move { "White" } else { "Black" };
        println!("{mover_label} to move.");

        let is_human_turn = white_to_move == human_is_white;

        if let Some(ps) = placement {
            let succs = opening::successors(&ps);
            if succs.is_empty() {
                println!("{mover_label} has no placement available -- this shouldn't happen.");
                break;
            }

            let chosen = if is_human_turn {
                loop {
                    print!("Enter placement (e.g. 'a1'): ");
                    io::stdout().flush()?;
                    let Some(line) = lines.next() else {
                        return Ok(());
                    };
                    let line = line?;
                    let Some(p) = board::parse_point(line.trim()) else {
                        println!("Could not parse '{line}'.");
                        continue;
                    };
                    let matches: Vec<_> = succs
                        .iter()
                        .filter(|s| (s.pos.black() & !ps.pos.white()) & (1 << p) != 0)
                        .collect();
                    if matches.is_empty() {
                        println!("Illegal placement at {line}.");
                        continue;
                    }
                    if matches.len() == 1 {
                        break *matches[0];
                    }
                    println!("Mill closed! Choose a stone to capture:");
                    for (i, m) in matches.iter().enumerate() {
                        let captured = (ps.pos.black() & !m.pos.white()).trailing_zeros() as usize;
                        println!("  {}: capture {}", i + 1, board::point_name(captured));
                    }
                    let idx = ask_choice(&mut lines, matches.len())?;
                    break *matches[idx];
                }
            } else {
                let choice =
                    play::best_placement_move(&ps, &db, &mut tt).expect("succs is nonempty");
                println!("Engine plays.");
                choice
            };

            white_to_move = !white_to_move;
            if chosen.placement_done() {
                movement = Some(chosen.pos);
                placement = None;
            } else {
                placement = Some(chosen);
            }
            continue;
        }

        // Movement phase.
        let pos = movement.unwrap();
        if pos.white_count() < 3 {
            println!("{mover_label} has fewer than three stones and loses.");
            break;
        }
        let Some(choice) = play::best_movement_move(pos, &db) else {
            println!("{mover_label} has no legal move and loses.");
            break;
        };

        let next = if is_human_turn {
            loop {
                print!("Enter move ('from to', e.g. 'a1 a4'): ");
                io::stdout().flush()?;
                let Some(line) = lines.next() else {
                    return Ok(());
                };
                let line = line?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                let (Some(from_s), Some(to_s)) = (parts.first(), parts.get(1)) else {
                    println!("Enter two squares separated by a space.");
                    continue;
                };
                let (Some(from), Some(to)) = (board::parse_point(from_s), board::parse_point(to_s))
                else {
                    println!("Could not parse squares.");
                    continue;
                };
                if pos.white() & (1 << from) == 0 {
                    println!("You have no stone at {from_s}.");
                    continue;
                }
                let succs = ninemm::movegen::successors(pos);
                let matches: Vec<_> = succs
                    .iter()
                    .filter(|s| {
                        let new_mover = s.black();
                        let old_mover_minus_from = pos.white() & !(1 << from);
                        new_mover & !old_mover_minus_from & (1 << to) != 0
                            && new_mover & (1 << from) == 0
                    })
                    .collect();
                if matches.is_empty() {
                    println!("Illegal move {from_s} -> {to_s}.");
                    continue;
                }
                if matches.len() == 1 {
                    break *matches[0];
                }
                println!("Mill closed! Choose a stone to capture:");
                for (i, m) in matches.iter().enumerate() {
                    let captured = (pos.black() & !m.white()).trailing_zeros() as usize;
                    println!("  {}: capture {}", i + 1, board::point_name(captured));
                }
                let idx = ask_choice(&mut lines, matches.len())?;
                break *matches[idx];
            }
        } else {
            println!(
                "Engine plays (value: {}).",
                describe_value(choice.value_code)
            );
            choice.successor
        };

        white_to_move = !white_to_move;
        movement = Some(next);
        if next.white_count() < 3 {
            println!();
            println!(
                "{}",
                play::render(if white_to_move {
                    next
                } else {
                    next.swap_colors()
                })
            );
            println!(
                "{} loses (fewer than three stones).",
                if white_to_move { "White" } else { "Black" }
            );
            break;
        }
    }
    Ok(())
}

fn describe_value(code: u16) -> String {
    if code == ninemm::retro::DRAW {
        "draw".to_string()
    } else if code.is_multiple_of(2) {
        format!("loss in {code}")
    } else {
        format!("win in {code}")
    }
}
