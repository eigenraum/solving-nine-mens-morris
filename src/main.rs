use clap::{Parser, Subcommand};

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
    }
    Ok(())
}
