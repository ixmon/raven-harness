//! Raven TUI — Simple agentic coding assistant backed by llama.cpp (or other OpenAI-compatible endpoints).
//!
//! Talks to localhost:8080 by default (llama.cpp / Qwen model).
//! Provides the agent with practical tools: filesystem ops, shell exec, web search, and page browsing.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;

extern crate raven_tui;

mod desktop;
mod app_state;
mod confirmation_dialog;
#[cfg(test)]
mod eval_scenarios;
mod input_dispatch;
mod input_handler;
mod plan_flow;
mod plan_sync;
mod plan_prompts;
mod key_edit;
mod event_loop;
mod keystore;
mod palette;
mod search;
mod settings_modal;
mod tui_app;
mod tui_render;

#[derive(Parser, Debug)]
#[command(
    name = "raven-tui",
    about = "Agentic coding TUI powered by local LLMs + tools"
)]
struct Args {
    /// OpenAI-compatible base URL (llama.cpp server, etc.)
    #[arg(long, env = "LLM_BASE_URL", default_value = "http://127.0.0.1:8080/v1")]
    base_url: String,

    /// Model name to use (whatever your llama.cpp server exposes)
    #[arg(long, env = "LLM_MODEL", default_value = "qwen2.5-coder")]
    model: String,

    /// Workspace directory the agent is allowed to operate in (defaults to current dir)
    #[arg(long, env = "WORKSPACE_DIR")]
    workspace: Option<PathBuf>,

    /// API key if your endpoint requires one (llama.cpp usually does not).
    /// Also checks OPENROUTER_API_KEY env var.
    #[arg(long, env = "LLM_API_KEY")]
    api_key: Option<String>,

    /// Max tool-use rounds per user turn (safety)
    #[arg(long, default_value_t = 10)]
    max_rounds: u32,

    /// Temperature for generation
    #[arg(long, default_value_t = 0.7)]
    temperature: f32,

    /// Max output tokens per LLM call
    #[arg(long, default_value_t = 4096)]
    max_tokens: u32,

    /// Override the context window size (in tokens) instead of probing the server.
    /// If not set, the server is probed at startup via /v1/models.
    #[arg(long, env = "LLM_CONTEXT_SIZE")]
    context_size: Option<u32>,

    /// Run a single prompt non-interactively and exit (useful for scripting).
    /// For large or complex prompts (e.g. full bug reports), prefer --prompt-file to avoid
    /// shell quoting problems.
    #[arg(long)]
    prompt: Option<String>,

    /// Read the prompt from a file (avoids shell quoting issues with large/complex prompts
    /// such as full SWE-bench bug reports). Mutually exclusive with --prompt.
    #[arg(long, value_name = "FILE")]
    prompt_file: Option<PathBuf>,

    /// Run without any persistent session. No conversation history is loaded,
    /// no goal/discovery tracking, no session injection block. Each invocation
    /// is a completely clean slate — ideal for evals and benchmarks.
    ///
    /// For clean evals you will usually want --fresh-session instead (see below).
    /// --no-session completely disables the rich SESSION CONTEXT, last_user_request
    /// (used for hard-safety "implies run" checks), goal tracking, file summaries,
    /// and the judge. The new session flags keep those benefits while giving you
    /// a dedicated clean history.
    #[arg(long)]
    no_session: bool,

    /// Use or create a named session (instead of the default one derived solely
    /// from the workspace path). This enables multiple resumable sessions for
    /// the same workspace, e.g. one for daily work and separate clean ones for
    /// evals. The session remains associated with the workspace for repo
    /// indexing and summaries.
    #[arg(long, value_name = "NAME")]
    session: Option<String>,

    /// Create a fresh unique session for this run (recommended for evals and
    /// benchmarks). Equivalent to --session with a generated unique name.
    /// This guarantees no history from previous runs, your personal coding,
    /// or self-modification of raven-tui leaks into the eval.
    #[arg(long)]
    fresh_session: bool,

    /// Execution approval mode for tools (writes, exec, etc.).
    /// One of: babysitter, springbreak, vegas, thunderdome (or via RAVEN_APPROVAL env).
    /// Thunderdome = eternal yolo (no prompts). SpringBreak = yolo for this session.
    #[arg(long, value_name = "MODE", env = "RAVEN_APPROVAL")]
    approval: Option<String>,

    /// Enable the full V2 nudge/judge/criteria logic (define_done + budgeted progress continues).
    /// Set automatically from scenario tests; can be disabled per-scenario with "disable_judge": true (or 1).
    #[arg(long)]
    enable_judge: bool,

    /// Hard wall-clock timeout for a single turn, in seconds.
    /// When elapsed, the agent stops regardless of judge/nudge state.
    /// No default for interactive mode (unlimited). Evals default to 600 (10 min).
    /// Also settable per-scenario via "max_duration" in the scenario JSON.
    #[arg(long, value_name = "SECS")]
    max_duration: Option<u64>,

    /// Print the full system prompt + injection block that the model would receive,
    /// then exit. Useful for debugging prompt construction without running inference.
    #[arg(long)]
    dump_prompt: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // ── Resolve all env vars once ─────────────────────────────────────────
    let mut flags = raven_tui::runtime::RuntimeFlags::from_env();
    // Merge CLI overrides
    if args.enable_judge {
        flags.enable_judge = true;
    }
    if let Some(secs) = args.max_duration {
        flags.max_duration_secs = Some(secs);
    } else if flags.is_eval && flags.max_duration_secs.is_none() {
        // Default safety timeout for evals: 10 minutes
        flags.max_duration_secs = Some(600);
    }
    let harness = raven_tui::runtime::EvalHarness::from_env();

    // Detect terminal color depth once. Downsample is applied centrally via
    // `PaletteBackend` in `tui_app::run`, so this just primes the cache.
    let color_depth = palette::init();
    if color_depth != palette::ColorDepth::True {
        eprintln!(
            "raven: terminal color depth detected as {:?} (RGB will be downsampled)",
            color_depth
        );
    }

    let eval_mode = raven_tui::eval_smoke::eval_enabled();
    let smoke_scenario = if eval_mode {
        let name = raven_tui::eval_smoke::scenario_name();
        Some(raven_tui::eval_smoke::load_smoke_scenario(&name)?)
    } else {
        None
    };

    let workspace = if eval_mode {
        raven_tui::eval_smoke::resolve_eval_workspace()?
    } else {
        args.workspace
            .unwrap_or_else(|| std::env::current_dir().expect("cannot get current dir"))
    };
    std::fs::create_dir_all(&workspace)?;

    let tool_backend = if let Some(ref scenario) = smoke_scenario {
        raven_tui::tools::ToolBackend::Mock(raven_tui::eval_smoke::mock_backend_for(scenario))
    } else {
        raven_tui::tools::ToolBackend::Real
    };

    let prompt = if let Some(p) = &args.prompt {
        if args.prompt_file.is_some() {
            anyhow::bail!("--prompt and --prompt-file are mutually exclusive");
        }
        Some(p.clone())
    } else if let Some(path) = &args.prompt_file {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read --prompt-file {}", path.display()))?;
        Some(content)
    } else {
        smoke_scenario.as_ref().map(|s| s.prompt.clone())
    };
    let is_interactive_tui = prompt.is_none();

    if eval_mode && is_interactive_tui {
        anyhow::bail!(
            "RAVEN_EVAL=1 requires --prompt or a smoke scenario with a prompt (set RAVEN_EVAL_SCENARIO)"
        );
    }

    // Resolve API key: --api-key > LLM_API_KEY > OPENROUTER_API_KEY
    let mut api_key = args
        .api_key
        .or_else(|| std::env::var("OPENROUTER_API_KEY").ok());

    // === New session / context management ( ~/.raven-hotel/ ) ===
    // One persistent session per workspace. Supports goal tracking (off by default; set RAVEN_GOAL_TRACKING=1 to enable), repo cache,
    // safe discovery, and a compact injection block for the model.
    //
    // By default the session id is derived from the workspace path (so the same
    // workspace always resumes the same session). For evals and to support
    // multiple resumable sessions, you can use --session NAME or --fresh-session.
    //
    // --no-session disables the session entirely (no injection block etc.).
    // For clean evals it is usually better to use --fresh-session so you still
    // get last_user_request (hard safety), goal tracking (if RAVEN_GOAL_TRACKING), summaries, the
    // SESSION CONTEXT injection, judge, etc., but with a brand new history.
    let no_session = args.no_session;
    let explicit_session = if args.fresh_session {
        Some(format!(
            "fresh-{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        ))
    } else {
        args.session.clone()
    };

    let mut sess = if no_session {
        None
    } else if let Some(name) = &explicit_session {
        Some(raven_tui::session::Session::init_named(&workspace, name)?)
    } else {
        Some(raven_tui::session::Session::init(&workspace)?)
    };

    if eval_mode {
        eprintln!(
            "eval: scenario={} workspace={} mock_tools={} no_session={}",
            smoke_scenario
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or("?"),
            workspace.display(),
            matches!(tool_backend, raven_tui::tools::ToolBackend::Mock(_)),
            no_session,
        );
    }

    if let Some(ref mut s) = sess {
        if let Some(mode_str) = &args.approval {
            if let Some(mode) = parse_approval_mode(mode_str) {
                s.meta.exec_approval_mode = mode;
                s.save_meta()?;
            } else {
                eprintln!(
                    "warning: unknown approval mode {:?}, falling back to env/default",
                    mode_str
                );
            }
        } else if approval_env_thunderdome() {
            s.meta.exec_approval_mode = raven_tui::session::ExecApprovalMode::Thunderdome;
            s.save_meta()?;
        }

        // For interactive use we offer the Cursor-style trust prompt.
        // This decides whether we do deep (but safe) indexing of the tree.
        if is_interactive_tui {
            let _ = raven_tui::session::ensure_repo_cache(s);
        } else if !s.meta.trusted {
            // Non-interactive: be conservative, do not index giant trees automatically.
            s.meta.trusted = false;
            s.save_meta()?;
        }
    }

    // Small one-time status (goes to stderr so it doesn't pollute --prompt output)
    if is_interactive_tui {
        if let Some(ref s) = sess {
            eprintln!(
                "raven session: {} | goal: {} | repo files: {} | trusted: {}",
                s.id,
                if flags.goal_tracking {
                    let g = &s.meta.current_goal;
                    let end = g.len().min(60);
                    let end = (0..=end)
                        .rev()
                        .find(|&i| g.is_char_boundary(i))
                        .unwrap_or(0);
                    &g[..end]
                } else {
                    "none"
                },
                s.meta.repo_cache.total_files_considered,
                s.meta.trusted
            );
        } else {
            eprintln!("raven: running without session (--no-session)");
        }
    }

    // === Encrypted endpoint vault ===
    let raven_home = dirs_next_home().join(".raven-hotel");
    let keystore_path = raven_home.join("endpoints.json");
    let mut ks = keystore::Keystore::load_or_create(&keystore_path).unwrap_or_else(|e| {
        eprintln!("warning: could not load endpoints.json: {}", e);
        keystore::Keystore::load_or_create(&keystore_path)
            .expect("FATAL: could not create ~/.raven-hotel/endpoints.json (check permissions)")
    });

    // If any endpoints have encrypted keys, prompt for vault password
    // Supports RAVEN_VAULT_PASSWORD env var for non-interactive / testing use
    if ks.has_encrypted_keys() {
        if let Some(pw) = &flags.vault_password {
            match ks.unlock(pw) {
                Ok(()) => eprintln!("Vault unlocked (from RAVEN_VAULT_PASSWORD)."),
                Err(e) => eprintln!(
                    "RAVEN_VAULT_PASSWORD failed: {}. Encrypted endpoints unavailable.",
                    e
                ),
            }
        } else if is_interactive_tui {
            eprintln!("Encrypted API keys found. Enter vault password:");
            for attempt in 1..=3u8 {
                match rpassword::read_password() {
                    Ok(pw) => match ks.unlock(&pw) {
                        Ok(()) => {
                            eprintln!("Vault unlocked.");
                            break;
                        }
                        Err(e) => {
                            if attempt < 3 {
                                eprintln!("Wrong password ({}/3): {}", attempt, e);
                            } else {
                                eprintln!("Failed to unlock vault after 3 attempts. Encrypted endpoints will be unavailable.");
                            }
                        }
                    },
                    Err(_) => {
                        eprintln!("Could not read password.");
                        break;
                    }
                }
            }
        }
    }

    let mut base_url = args.base_url.clone();
    let mut model = args.model.clone();
    if let Ok(Some(launch)) = ks.launch_endpoint() {
        base_url = launch.base_url;
        model = launch.model;
        if launch.api_key.is_some() {
            api_key = launch.api_key;
        }
    }

    let use_mock_llm = eval_mode && raven_tui::eval_smoke::mock_llm_enabled();
    let max_rounds = smoke_scenario
        .as_ref()
        .and_then(|s| s.max_rounds)
        .unwrap_or(args.max_rounds);
    // Scenario can override the wall-clock timeout
    if let Some(dur) = smoke_scenario.as_ref().and_then(|s| s.max_duration_secs) {
        flags.max_duration_secs = Some(dur);
    }
    let scenario_n_ctx = smoke_scenario.as_ref().and_then(|s| s.context_tokens);

    // === Context budget: probe the active endpoint (after launch override) ===
    let context_budget = if use_mock_llm {
        let n_ctx = scenario_n_ctx.unwrap_or(8192);
        eprintln!("eval: mock LLM — skipping server probe (n_ctx={n_ctx})");
        raven_tui::config::ContextBudget::from_context_tokens(n_ctx, max_rounds)
    } else if let Some(n_ctx) = args.context_size {
        let mut b = raven_tui::config::ContextBudget::from_context_tokens(n_ctx, args.max_rounds);
        b.source = raven_tui::config::ContextSource::CliOverride;
        b
    } else {
        match raven_tui::llm::probe_server(&base_url, &model, api_key.as_deref()).await {
            Some(probe) => {
                if probe.model_id != model {
                    eprintln!(
                        "model: auto-detected {} (configured: {})",
                        probe.model_id, model
                    );
                    model = probe.model_id;
                }
                raven_tui::config::ContextBudget::from_context_tokens(
                    probe.context_tokens,
                    args.max_rounds,
                )
            }
            None => {
                eprintln!(
                    "warning: could not probe {}/models for model {:?}; using 8192 token default",
                    base_url.trim_end_matches('/'),
                    model
                );
                raven_tui::config::ContextBudget::default_fallback()
            }
        }
    };

    eprintln!(
        "context: {} tokens ({}) → budget: {} bytes/result, {} lines/read, {} rounds",
        context_budget.context_tokens,
        context_budget.source,
        context_budget.tool_result_bytes,
        context_budget.read_line_limit,
        max_rounds,
    );

    if let Some(prompt) = prompt {
        // Non-interactive single shot — uses the SAME agent loop as the TUI
        // (streaming, nudges, auto-continue) via drive_turn().
        if let Some(ref mut s) = sess {
            let _ = s.set_last_user_request(&prompt);
        }

        // For evals (SWE-bench etc.) the launcher can tell us exactly where
        // the project python lives via these env vars. We surface a small
        // "stock" eval context so the agent doesn't waste turns on "python not
        // found" or wrong interpreter.
        let effective_prompt = if harness.eval_python.is_some() || harness.eval_python3.is_some() {
            let mut note = String::from("## Evaluation Harness Context\n");
            if let Some(_p) = &harness.eval_python {
                // note.push_str(&format!("Use this exact Python interpreter for the project under test: {}\n", _p));
            }
            if let Some(_p) = &harness.eval_python3 {
                // note.push_str(&format!("python3 equivalent (if needed): {}\n", _p));
            }
            /*
            note.push_str("Prefer the python from the environment the harness launched you in. ");
            note.push_str("Bare `python` or `python3` should resolve correctly because of PATH, but use the full path above if in doubt.\n\n");
            */
            note.push_str("You are an expert software developer, keep working until you have fixed the bug described below.\n");
            note.push_str("Early in the task, call the `define_done` tool **once** with a clear, precise definition of what \"done\" / success looks like, derived directly from the task description below. Only the judge will clear it when fulfilled.\n\n");
            note.push_str("## Task / Bug Report\n");
            note.push_str(&prompt);
            note
        } else {
            prompt.clone()
        };
        let tools_enabled = !smoke_scenario.as_ref().is_some_and(|s| s.disable_tools);
        let model_label = model.clone();
        let c = raven_tui::config::Config {
            base_url,
            model,
            api_key,
            workspace: workspace.clone(),
            temperature: args.temperature,
            max_tokens: args.max_tokens,
            max_rounds,
            prebuilt_session: sess,
            context_budget: context_budget.clone(),
            tool_backend,
            tools_enabled,
            enable_judge: flags.enable_judge,
            flags: flags.clone(),
            harness: harness.clone(),
        };
        let chat_backend = if use_mock_llm {
            let scenario = smoke_scenario
                .as_ref()
                .expect("RAVEN_EVAL_MOCK_LLM requires a smoke scenario");
            raven_tui::chat_backend::ChatBackend::Mock(
                raven_tui::eval_smoke::mock_chat_backend_for(scenario),
            )
        } else {
            raven_tui::chat_backend::ChatBackend::http(c.clone())
        };
        let mut app = raven_tui::agent::Agent::new(c, chat_backend);

        // Each --prompt / --prompt-file invocation (swebench, smoke evals, direct headless use)
        // is an independent task against a freshly checked-out workspace (run_instance.sh etc.
        // do `git reset --hard $base + clean -fdx`).
        //
        // Use --fresh-session (or explicit --session NAME) to get a dedicated clean
        // session for the run. This prevents pollution from prior tests, your
        // personal coding, or raven-tui self-modification, while still giving the
        // model the full SESSION CONTEXT, last_user_request (hard safety),
        // summaries, judge, etc.
        app.reset();

        // --dump-prompt: print what the model would see and exit
        if args.dump_prompt {
            app.record_user_request(&effective_prompt);
            let msgs = app.dump_prompt();
            for (i, m) in msgs.iter().enumerate() {
                let content = m.content.as_deref().unwrap_or("");
                let tc = if let Some(tcs) = &m.tool_calls {
                    format!(
                        " [tool_calls: {}]",
                        tcs.iter()
                            .map(|t| t.function.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    String::new()
                };
                println!("═══ Message {} ═══ role={}{}", i, m.role, tc);
                println!("{}", content);
                println!();
            }
            eprintln!(
                "dump-prompt: {} messages, {} total bytes",
                msgs.len(),
                msgs.iter()
                    .map(|m| m.content.as_ref().map_or(0, |c| c.len()))
                    .sum::<usize>()
            );
            return Ok(());
        }

        let turn_started = std::time::Instant::now();

        // Use drive_turn with HeadlessObserver — same loop as TUI
        let mut observer = raven_tui::agent_driver::HeadlessObserver;
        let result =
            raven_tui::agent_driver::drive_turn(&mut app, &effective_prompt, &mut observer).await?;

        if let Some(out) = &harness.metrics_out {
            let _ = raven_tui::eval_metrics::write_turn_metrics(
                out.as_path(),
                &result,
                turn_started.elapsed().as_millis() as u64,
                &model_label,
                max_rounds,
            );
        }
        if let Some(ref scenario) = smoke_scenario {
            raven_tui::eval_smoke::assert_smoke_result(scenario, &result, &workspace)?;
            raven_tui::eval_smoke::assert_smoke_session_log(scenario, &workspace)?;
            raven_tui::eval_metrics::assert_smoke_metrics(scenario, &result, &harness)?;
            eprintln!("eval: PASS {:?}", scenario.name);
        }
        println!("{}", result.final_text);
        if !result.actions.is_empty() {
            println!("\n[actions]");
            for a in result.actions {
                println!("  {} → {}", a.tool, a.summary);
            }
        }
        return Ok(());
    }

    // Interactive TUI path
    let c = raven_tui::config::Config {
        base_url,
        model,
        api_key,
        workspace: workspace.clone(),
        temperature: args.temperature,
        max_tokens: args.max_tokens,
        max_rounds: args.max_rounds,
        prebuilt_session: sess,
        context_budget,
        tool_backend,
        tools_enabled: true,
        enable_judge: flags.enable_judge,
        flags: flags.clone(),
        harness,
    };

    // --dump-prompt without --prompt: show the system prompt + injection block
    if args.dump_prompt {
        let chat_backend = raven_tui::chat_backend::ChatBackend::http(c.clone());
        let app = raven_tui::agent::Agent::new(c, chat_backend);
        let msgs = app.dump_prompt();
        for (i, m) in msgs.iter().enumerate() {
            let content = m.content.as_deref().unwrap_or("");
            println!("═══ Message {} ═══ role={}", i, m.role);
            println!("{}", content);
            println!();
        }
        eprintln!(
            "dump-prompt: {} messages (no user prompt — use --prompt to include one)",
            msgs.len()
        );
        return Ok(());
    }

    // Interactive TUI: do not print anything to stdout before entering alternate screen.
    // Trust prompt + any eprintln above already happened on the normal terminal.
    let chat_backend = raven_tui::chat_backend::ChatBackend::http(c.clone());
    tui_app::run(c, chat_backend, ks).await
}

/// Non-interactive evals (SWE-bench, harness smoke) auto-approve tool side effects.
fn approval_env_thunderdome() -> bool {
    ["RAVEN_APPROVAL", "RAVEN_EVAL_APPROVAL"]
        .into_iter()
        .any(|key| {
            std::env::var(key)
                .ok()
                .is_some_and(|v| v.eq_ignore_ascii_case("thunderdome"))
        })
}

fn parse_approval_mode(s: &str) -> Option<raven_tui::session::ExecApprovalMode> {
    match s.to_ascii_lowercase().as_str() {
        "babysitter" | "ask" | "default" => Some(raven_tui::session::ExecApprovalMode::Babysitter),
        "springbreak" | "spring-break" | "spring_break" | "yolo-session" => {
            Some(raven_tui::session::ExecApprovalMode::SpringBreak)
        }
        "vegas" => Some(raven_tui::session::ExecApprovalMode::Vegas),
        "thunderdome" | "yolo" | "eternal" | "full-yolo" => {
            Some(raven_tui::session::ExecApprovalMode::Thunderdome)
        }
        _ => None,
    }
}

/// Get home directory (simple fallback).
fn dirs_next_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}
