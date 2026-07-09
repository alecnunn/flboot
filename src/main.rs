mod root;
mod log;
mod model;
mod codegen;
mod objdiff;
mod fetch;
mod bootstrap;
mod dev;
mod claims;
mod manifest;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "fl", version, about = "Unified bootstrap + dev-loop tool for the Freelancer decomp")]
struct Cli {
    #[arg(long, default_value = "052103_release_1149_Ipatch_ver1254", global = true)]
    config_id: String,

    #[arg(long, global = true)]
    config: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Full setup: fetch tools, fetch+verify orig binaries, split libraries, regen build files
    Bootstrap {
        #[arg(long)]
        delink: Option<String>,
        #[arg(long)]
        objdiff_cli: Option<String>,
        #[arg(long)]
        skip_delink: bool,
        #[arg(long)]
        only: Vec<String>,
    },
    /// Compile object(s) with the exact flags ninja would use
    Build { units: Vec<String> },
    /// Regenerate one unit's target object from its delink/split config
    Delink { unit: String },
    /// Rename target symbol(s) and re-delink
    Claim { unit: String, renames: Vec<String> },
    /// Show original->claimed symbol mapping reconstructed from git history
    Claims { units: Vec<String> },
    /// Target-vs-ours disassembly diff for one function, or all functions in the unit if omitted
    Diff { unit: String, symbol: Option<String> },
    /// Target-only disassembly listing for one or more symbols
    Dis { unit: String, symbols: Vec<String> },
    /// Match percentage report
    Progress { units: Vec<String> },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let repo_root = root::find_repo_root()?;
    std::env::set_current_dir(&repo_root)?;

    match cli.command {
        Commands::Bootstrap { delink, objdiff_cli, skip_delink, only } => bootstrap::run_bootstrap(
            &cli.config_id,
            cli.config.as_deref(),
            delink.as_deref(),
            objdiff_cli.as_deref(),
            skip_delink,
            &only,
        ),
        Commands::Build { units } => dev::cmd_build(&cli.config_id, &units),
        Commands::Delink { unit } => dev::cmd_delink(&cli.config_id, &unit),
        Commands::Claim { unit, renames } => dev::cmd_claim(&cli.config_id, &unit, &renames),
        Commands::Claims { units } => claims::cmd_claims(&cli.config_id, &units),
        Commands::Diff { unit, symbol } => dev::cmd_diff(&cli.config_id, &unit, symbol.as_deref()),
        Commands::Dis { unit, symbols } => dev::cmd_dis(&cli.config_id, &unit, &symbols),
        Commands::Progress { units } => dev::cmd_progress(&cli.config_id, &units),
    }
}
