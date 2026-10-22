//! Startup banner, styling, and usage formatting for the aegis CLI.

/// Terminal styling constants using standard ANSI escape sequences.
pub mod style {
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const CYAN: &str = "\x1b[36m";
    pub const BLUE: &str = "\x1b[34m";
    pub const GREEN: &str = "\x1b[32m";
    pub const RESET: &str = "\x1b[0m";
}

/// ASCII Art banner for AEGIS.
const BANNER_ART: &str = r"
    ___    ______ ____ _____ _____
   /   |  / ____//  _// ___// ___/
  / /| | / __/   / / / (_ //\__ \ 
 / ___ |/ /___ _/ / / /__//___/ / 
/_/  |_/_____//___/ \___//____/  
";

/// Prints the stylized ASCII startup banner with version and tagline.
pub fn print_banner() {
    println!(
        "{}{}{}",
        style::CYAN,
        BANNER_ART.trim_matches('\n'),
        style::RESET
    );
    println!(
        " {}Autonomous Software Engineering Agent Platform{} [v{}]\n",
        style::DIM,
        style::RESET,
        env!("CARGO_PKG_VERSION")
    );
}

/// Prints a well-structured and colored help message.
pub fn print_help() {
    print_banner();
    println!("{}USAGE:{}", style::BOLD, style::RESET);
    println!("  aegis <COMMAND> [OPTIONS]\n");

    println!("{}COMMANDS:{}", style::BOLD, style::RESET);
    let commands = [
        (
            "run",
            "Solve a single issue from GitHub (owner/repo#123) or local file",
        ),
        ("batch", "Process a list of issues sequentially from a file"),
        (
            "eval",
            "Run evaluation benchmarks on test sets (supports --replay & --no-pr)",
        ),
        (
            "review",
            "Record manual review verdicts and KPI metrics for a run",
        ),
        ("report", "Display KPI report and benchmark statistics"),
    ];

    for (name, desc) in commands {
        println!(
            "  {GREEN}{name:<8}{RESET} {desc}",
            GREEN = style::GREEN,
            RESET = style::RESET
        );
    }

    println!("\n{}EXAMPLES:{}", style::BOLD, style::RESET);
    println!(
        "  {DIM}# Run a single issue:{RESET}\n  aegis run --config config/poc.toml --issue scrapy/scrapy#6123\n",
        DIM = style::DIM,
        RESET = style::RESET,
    );
    println!(
        "  {DIM}# Run an eval set with dry-run replay:{RESET}\n  aegis eval --config config/poc.toml --set eval/issues --replay --no-pr\n",
        DIM = style::DIM,
        RESET = style::RESET,
    );
    println!(
        "  {DIM}# View KPI metrics report:{RESET}\n  aegis report --config config/poc.toml\n",
        DIM = style::DIM,
        RESET = style::RESET,
    );
    println!(
        "For more flags and details, see documentation or provide --config with relevant command."
    );
}

pub fn print_dashboard(config_path: &str, repo_key: &str, language: &str, core_model: &str) {
    println!(
        "{DIM}┌─ aegis execution dashboard ──────────────────────────────┐{RESET}",
        DIM = style::DIM,
        RESET = style::RESET,
    );
    println!(
        "{DIM}│{RESET}  {BLUE}Config:{RESET}   {config_path}",
        DIM = style::DIM,
        RESET = style::RESET,
        BLUE = style::BLUE,
    );
    println!(
        "{DIM}│{RESET}  {BLUE}Target:{RESET}   {repo_key} ({language})",
        DIM = style::DIM,
        RESET = style::RESET,
        BLUE = style::BLUE,
    );
    println!(
        "{DIM}│{RESET}  {BLUE}Model:{RESET}    {core_model}",
        DIM = style::DIM,
        RESET = style::RESET,
        BLUE = style::BLUE,
    );
    println!(
        "{DIM}└──────────────────────────────────────────────────────────┘{RESET}\n",
        DIM = style::DIM,
        RESET = style::RESET,
    );
}
