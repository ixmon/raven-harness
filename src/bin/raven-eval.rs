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
    /// Non-interactive profile: quick | local | full | swebench-smoke | swebench-live | easy-bench-live
    /// (single tests: use --test <name> [--interactive] [--live])
    #[arg(long)]
    profile: Option<String>,

    // Note: --test <name> --interactive will set up the workspace + prompt and launch the full interactive TUI
    // (with --temperature 1 by default for these runs).

    /// Live Raven agent for a single SWE-bench instance (--run / --test <instance_id> only)
    #[arg(long)]
    live: bool,

    /// Launch interactive TUI for the test with the prompt (env context + test prompt) pre-filled in the input box.
    /// Press Enter to start the agent. Example: raven-eval --test marshmallow-code__marshmallow-1343 --interactive
    #[arg(long)]
    interactive: bool,

    /// List registry and exit
    #[arg(long)]
    list: bool,

    /// Open AI Shell REPL (requires reachable LLM)
    #[arg(long)]
    ai_shell: bool,

    /// Explain the last run using local LLM
    #[arg(long)]
    explain: bool,

    /// Run or launch a single test/scenario id (e.g. easy-hello-world, marshmallow-code__marshmallow-1343).
    /// Use --test or --run (alias).
    #[arg(long, alias = "run")]
    test: Option<String>,

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

    if let Some(id) = args.test.clone() {
        // --test (or --run via alias) was provided
        let mut state = load_state()?;
        let is_swebench = raven_tui::eval_operator::swebench::is_instance_id(
            &runner.manifest_dir,
            &id,
        );
        let is_easy = id.starts_with("easy-");
        if args.live && !is_swebench && !is_easy {
            anyhow::bail!("--live requires a SWE-bench instance id (see evals/swebench/instances/) or easy-* id");
        }

        if args.interactive {
            runner.launch_interactive(&id)?;
            return Ok(());
        }

        let profile = if args.live {
            if is_easy { "easy-bench-live".into() } else { "swebench-live".into() }
        } else if is_swebench {
            "swebench-smoke".into()
        } else if is_easy {
            "easy-bench-live".into()
        } else {
            format!("single:{id}")
        };
        let summary = runner.run_ids(&profile, &[id], &mut state)?;
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
            anyhow::bail!(
                "unknown profile {profile:?} (use quick, local, full, swebench-smoke, swebench-live, or easy-bench-live). For single tests use --test <name> [--interactive]"
            );
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