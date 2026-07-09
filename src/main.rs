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

use clap::{Parser, Subcommand, ValueEnum};

/// Whether to draw connected branch arrows in the left gutter.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum BranchStyle {
    /// Draw a connected arrow from each branch source to its destination.
    Arrows,
    /// Omit branch annotations entirely.
    None,
}

/// When to colorize output. `auto` colorizes only when stdout is a terminal
/// and NO_COLOR is unset; `always` is for piping into a pager like `less -R`.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ColorChoice {
    Auto,
    Always,
    Never,
}

impl ColorChoice {
    fn enabled(self) -> bool {
        match self {
            ColorChoice::Auto => log::color_enabled(),
            ColorChoice::Always => true,
            ColorChoice::Never => false,
        }
    }
}

#[derive(Parser)]
#[command(name = "fl", version, about = "Unified bootstrap + dev-loop tool for the Freelancer decomp")]
struct Cli {
    #[arg(long, default_value = "052103_release_1149_Ipatch_ver1254", global = true)]
    config_id: String,

    #[arg(long, global = true)]
    config: Option<String>,

    #[arg(long, value_enum, default_value = "auto", global = true)]
    color: ColorChoice,

    #[arg(long, value_enum, default_value = "arrows", global = true)]
    branches: BranchStyle,

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
        Commands::Diff { unit, symbol } => dev::cmd_diff(
            &cli.config_id,
            &unit,
            symbol.as_deref(),
            cli.color.enabled(),
            cli.branches == BranchStyle::Arrows,
        ),
        Commands::Dis { unit, symbols } => dev::cmd_dis(
            &cli.config_id,
            &unit,
            &symbols,
            cli.color.enabled(),
            cli.branches == BranchStyle::Arrows,
        ),
        Commands::Progress { units } => dev::cmd_progress(&cli.config_id, &units),
    }
}
