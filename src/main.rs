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
                if sub.w >= 8 || (sub.w, sub.b) == (SubspaceId::new(3, 3).w, SubspaceId::new(3, 3).b)
                {
                    println!("subspace {}-{}: {sz}", sub.w, sub.b);
                }
            }
            println!("total slots across all 49 subspaces: {total}");
            println!("solve order (28 unordered pairs): {:?}", index::solve_order());
        }
        Commands::Solve { dir, max_total } => {
            ninemm::orchestrate::solve_all(&dir, max_total)?;
        }
    }
    Ok(())
}
