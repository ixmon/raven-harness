//! Interactive numbered menu for `raven-eval`.

use anyhow::Result;
use std::io::{self, Write};

use super::ai_shell::{explain_last_run, run_repl};
use super::probe::{format_status, LlmStatus};
use super::registry::{load_registry, list_text};
use super::runner::{Runner, print_summary};
use super::state::{format_last_run, load_state, save_state};

pub fn run_interactive(llm: &LlmStatus, runner: &Runner) -> Result<()> {
    let mut state = load_state()?;

    loop {
        println!();
        println!("raven-eval — Raven harness operator");
        println!("{}", "─".repeat(36));
        println!("{}", format_status(llm));
        println!("{}", format_last_run(&state));
        println!();
        println!("  1) Quick check      replay + mock smoke (~15s, no LLM)");
        println!("  2) Local smoke      quick + live smoke_ping (needs LLM)");
        println!("  3) Full harness     quick + all live smokes if LLM up");
        println!("  4) List tests       show registry");
        println!("  5) Re-run last      repeat last profile");
        println!("  6) View last results");
        if llm.reachable {
            println!("  7) Explain last run (AI)");
            println!("  8) AI Shell         freeform questions");
        } else {
            println!("  7) Explain last run (AI)     [unavailable — no LLM]");
            println!("  8) AI Shell                  [unavailable — no LLM]");
        }
        println!("  q) Quit");
        println!();
        print!("Choice [1]: ");
        io::stdout().flush()?;

        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let choice = line.trim();
        let choice = if choice.is_empty() { "1" } else { choice };

        match choice {
            "1" => {
                let s = runner.run_profile("quick", &mut state)?;
                print_summary(&s);
                maybe_offer_live_ping(llm, runner, &mut state)?;
            }
            "2" => {
                if !llm.reachable {
                    println!("LLM not reachable — cannot run local smoke.");
                    continue;
                }
                let s = runner.run_profile("local", &mut state)?;
                print_summary(&s);
            }
            "3" => {
                let s = runner.run_profile("full", &mut state)?;
                print_summary(&s);
                if !llm.reachable {
                    println!("(live scenarios skipped — LLM unreachable)");
                }
            }
            "4" => {
                let reg = load_registry()?;
                print!("{}", list_text(&reg));
            }
            "5" => {
                let profile = state
                    .last_profile
                    .clone()
                    .unwrap_or_else(|| "quick".into());
                let s = runner.run_profile(&profile, &mut state)?;
                print_summary(&s);
            }
            "6" => {
                if let Some(r) = state.runs.last() {
                    println!("{}", serde_json::to_string_pretty(r)?);
                    println!("Logs: {}", r.log_dir.display());
                } else {
                    println!("No runs yet.");
                }
            }
            "7" => {
                if !llm.reachable {
                    println!("LLM not reachable at {}", llm.base_url);
                    continue;
                }
                explain_last_run(llm)?;
            }
            "8" => {
                if !llm.reachable {
                    println!("LLM not reachable at {}", llm.base_url);
                    continue;
                }
                run_repl(llm, runner)?;
            }
            "q" | "Q" => break,
            _ => println!("Unknown choice: {choice}"),
        }
    }
    save_state(&state)?;
    Ok(())
}

fn maybe_offer_live_ping(
    llm: &LlmStatus,
    runner: &Runner,
    state: &mut super::state::OperatorState,
) -> Result<()> {
    if !llm.reachable {
        return Ok(());
    }
    print!("Run live smoke_ping too? [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let line = line.trim().to_lowercase();
    if line == "y" || line == "yes" {
        let s = runner.run_ids("live_ping", &["smoke_ping".into()], state)?;
        print_summary(&s);
    }
    Ok(())
}