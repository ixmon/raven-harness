//! Freeform eval operator REPL with optional local LLM assistance.

use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::json;
use std::io::{self, Write};
use std::time::Duration;

use super::probe::LlmStatus;
use super::registry::{load_registry, list_text};
use super::runner::{Runner, print_summary};
use super::state::{load_state, last_run, OperatorState};

pub fn explain_last_run(llm: &LlmStatus) -> Result<()> {
    if !llm.reachable {
        anyhow::bail!("LLM not reachable — cannot explain last run");
    }
    let state = load_state()?;
    let Some(run) = last_run(&state) else {
        println!("No previous run to explain.");
        return Ok(());
    };
    let prompt = format!(
        "Explain this harness eval run to the operator. Be concise and actionable.\n\n{}",
        serde_json::to_string_pretty(run)?
    );
    let answer = ask_llm(llm, &prompt)?;
    println!("\n{answer}\n");
    Ok(())
}

pub fn run_repl(llm: &LlmStatus, runner: &Runner) -> Result<()> {
    if !llm.reachable {
        anyhow::bail!("AI Shell unavailable — LLM not reachable at {}", llm.base_url);
    }

    println!("\nAI Shell — /help for commands, /quit to exit\n");
    let stdin = io::stdin();
    loop {
        print!("eval> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if handle_command(line, llm, runner)? {
            break;
        }
    }
    Ok(())
}

fn handle_command(line: &str, llm: &LlmStatus, runner: &Runner) -> Result<bool> {
    if line == "/quit" || line == "/exit" || line == "q" {
        return Ok(true);
    }
    if line == "/help" {
        print_help();
        return Ok(false);
    }
    if line == "/list" {
        let reg = load_registry()?;
        print!("{}", list_text(&reg));
        return Ok(false);
    }
    if line == "/last" {
        let state = load_state()?;
        match last_run(&state) {
            None => println!("No runs yet."),
            Some(r) => println!("{}", serde_json::to_string_pretty(r)?),
        }
        return Ok(false);
    }
    if let Some(rest) = line.strip_prefix("/compare") {
        compare_runs(rest.trim())?;
        return Ok(false);
    }
    if let Some(id) = line.strip_prefix("/run ") {
        let id = id.trim();
        confirm_and_run(id, runner)?;
        return Ok(false);
    }

    let answer = ask_llm_with_tools(llm, line)?;
    println!("\n{answer}\n");

    if let Some(id) = detect_run_suggestion(&answer) {
        println!("Detected run suggestion: {id}");
        confirm_and_run(&id, runner)?;
    }
    Ok(false)
}

fn print_help() {
    println!(
        r#"Commands:
  /list              List all tests and scenarios
  /last              Show last run summary (JSON)
  /compare [run_id]  Compare last two runs (or vs run_id)
  /run <id>          Run a test/scenario (with confirmation)
  /help              This help
  /quit              Exit

Freeform questions use the local LLM with registry + run history context.
"#
    );
}

fn compare_runs(run_id: &str) -> Result<()> {
    let state = load_state()?;
    if state.runs.len() < 2 && run_id.is_empty() {
        println!("Need at least two runs to compare.");
        return Ok(());
    }
    let a = state.runs.last().expect("last");
    let b = if run_id.is_empty() {
        &state.runs[state.runs.len() - 2]
    } else {
        state
            .runs
            .iter()
            .find(|r| r.run_id == run_id)
            .ok_or_else(|| anyhow::anyhow!("run_id not found: {run_id}"))?
    };
    println!("Compare {} vs {}", a.run_id, b.run_id);
    println!("  {} passed={}", a.profile, a.passed);
    println!("  {} passed={}", b.profile, b.passed);
    for id in a
        .results
        .iter()
        .map(|r| &r.id)
        .collect::<std::collections::BTreeSet<_>>()
    {
        let ap = a.results.iter().find(|r| &r.id == id).map(|r| r.passed);
        let bp = b.results.iter().find(|r| &r.id == id).map(|r| r.passed);
        if ap != bp {
            println!("  Δ {id}: {:?} → {:?}", ap, bp);
        }
    }
    Ok(())
}

fn confirm_and_run(id: &str, runner: &Runner) -> Result<()> {
    print!("Run {id}? [Y/n] ");
    io::stdout().flush()?;
    let mut ans = String::new();
    io::stdin().read_line(&mut ans)?;
    let ans = ans.trim().to_lowercase();
    if ans == "n" || ans == "no" {
        println!("Cancelled.");
        return Ok(());
    }
    let mut state: OperatorState = load_state()?;
    let summary = runner.run_ids(&format!("single:{id}"), &[id.to_string()], &mut state)?;
    print_summary(&summary);
    Ok(())
}

fn build_context() -> Result<String> {
    let reg = load_registry()?;
    let state = load_state()?;
    let mut ctx = String::new();
    ctx.push_str("## Registry\n");
    ctx.push_str(&list_text(&reg));
    ctx.push_str("\n## Recent runs\n");
    for r in state.runs.iter().rev().take(5) {
        ctx.push_str(&format!(
            "- {} profile={} passed={} items={}\n",
            r.run_id,
            r.profile,
            r.passed,
            r.results.len()
        ));
    }
    Ok(ctx)
}

fn ask_llm(llm: &LlmStatus, user: &str) -> Result<String> {
    ask_llm_with_context(llm, user, "")
}

fn ask_llm_with_tools(llm: &LlmStatus, user: &str) -> Result<String> {
    let ctx = build_context()?;
    ask_llm_with_context(llm, user, &ctx)
}

fn ask_llm_with_context(llm: &LlmStatus, user: &str, context: &str) -> Result<String> {
    let base = llm.base_url.trim_end_matches('/');
    let url = format!("{base}/chat/completions");
    let model = llm
        .model_hint
        .clone()
        .or_else(|| std::env::var("LLM_MODEL").ok())
        .unwrap_or_else(|| "local-model".into());

    let system = format!(
        "You are the Raven harness eval operator assistant. Answer questions about tests, \
         scenarios, and run history. Do not invent tests — only reference the registry below. \
         To run a test, tell the user to use /run <id> or say RUN: <id>.\n\n{context}"
    );

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "temperature": 0.2,
        "max_tokens": 1024,
        "stream": false,
    });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .context("LLM chat request")?;
    if !resp.status().is_success() {
        anyhow::bail!("LLM error {}: {}", resp.status(), resp.text().unwrap_or_default());
    }
    let data: serde_json::Value = resp.json()?;
    let content = data
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("(empty response)")
        .to_string();
    Ok(content)
}

fn detect_run_suggestion(answer: &str) -> Option<String> {
    for line in answer.lines() {
        if let Some(rest) = line.strip_prefix("RUN:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}