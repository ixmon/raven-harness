//! Raven harness eval operator — menus, profiles, optional AI Shell.

use anyhow::Result;
use clap::Parser;
use raven_tui::eval_operator::{
    menu, probe::{format_status, probe_llm},
    registry::{list_text, load_registry, profile_ids},
    runner::{print_summary, Runner},
    state::load_state,
};

#[derive(Parser, Debug)]
#[command(
    name = "raven-eval",
    about = "Raven harness eval operator — deterministic menus, optional local LLM"
)]
struct Args {
    /// Non-interactive profile: quick | local | full | swebench-smoke
    #[arg(long)]
    profile: Option<String>,

    /// List registry and exit
    #[arg(long)]
    list: bool,

    /// Open AI Shell REPL (requires reachable LLM)
    #[arg(long)]
    ai_shell: bool,

    /// Explain the last run using local LLM
    #[arg(long)]
    explain: bool,

    /// Run a single test/scenario id
    #[arg(long)]
    run: Option<String>,

    /// OpenAI-compatible base URL for AI features and live smoke
    #[arg(long, env = "LLM_BASE_URL", default_value = "http://127.0.0.1:8080/v1")]
    base_url: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let llm = probe_llm(&args.base_url);
    let mut runner = Runner::new();
    runner.llm_base_url = args.base_url.clone();

    if args.list {
        let reg = load_registry()?;
        print!("{}", list_text(&reg));
        return Ok(());
    }

    if let Some(id) = args.run {
        let mut state = load_state()?;
        let summary = runner.run_ids(&format!("single:{id}"), &[id], &mut state)?;
        print_summary(&summary);
        if !summary.passed {
            std::process::exit(1);
        }
        return Ok(());
    }

    if args.explain {
        raven_tui::eval_operator::ai_shell::explain_last_run(&llm)?;
        return Ok(());
    }

    if args.ai_shell {
        raven_tui::eval_operator::ai_shell::run_repl(&llm, &runner)?;
        return Ok(());
    }

    if let Some(profile) = args.profile {
        if profile_ids(&profile).is_err() {
            anyhow::bail!("unknown profile {profile:?} (use quick, local, full, or swebench-smoke)");
        }
        let mut state = load_state()?;
        let summary = runner.run_profile(&profile, &mut state)?;
        print_summary(&summary);
        if !summary.passed {
            std::process::exit(1);
        }
        return Ok(());
    }

    // No args → interactive menu
    eprintln!("{}", format_status(&llm));
    menu::run_interactive(&llm, &runner)
}