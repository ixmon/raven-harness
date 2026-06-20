//! Raven TUI — Simple agentic coding assistant backed by llama.cpp (or other OpenAI-compatible endpoints).
//!
//! Talks to spark.home.arpa:8080 by default (Qwen model).
//! Provides the agent with practical tools: filesystem ops, shell exec, web search, and page browsing.

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

mod agent;
mod config;
mod input_dispatch;
mod keystore;
mod llm;
mod palette;
mod search;
mod session;
mod settings_modal;
mod tools;
mod tui_app;
mod tui_render;

#[derive(Parser, Debug)]
#[command(name = "raven-tui", about = "Agentic coding TUI powered by local LLMs + tools")]
struct Args {
    /// OpenAI-compatible base URL (llama.cpp server, etc.)
    #[arg(long, env = "LLM_BASE_URL", default_value = "http://spark.home.arpa:8080/v1")]
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

    /// Run a single prompt non-interactively and exit (useful for scripting)
    #[arg(long)]
    prompt: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Detect terminal color depth once. Downsample is applied centrally via
    // `PaletteBackend` in `tui_app::run`, so this just primes the cache.
    let color_depth = palette::init();
    if color_depth != palette::ColorDepth::True {
        eprintln!("raven: terminal color depth detected as {:?} (RGB will be downsampled)", color_depth);
    }

    let workspace = args
        .workspace
        .unwrap_or_else(|| std::env::current_dir().expect("cannot get current dir"));
    std::fs::create_dir_all(&workspace)?;

    // Resolve API key: --api-key > LLM_API_KEY > OPENROUTER_API_KEY
    let api_key = args.api_key
        .or_else(|| std::env::var("OPENROUTER_API_KEY").ok());

    // === Context budget: probe server or use override ===
    let context_budget = if let Some(n_ctx) = args.context_size {
        let mut b = config::ContextBudget::from_context_tokens(n_ctx, args.max_rounds);
        b.source = config::ContextSource::CliOverride;
        b
    } else {
        match llm::probe_context_size(&args.base_url, &args.model, api_key.as_deref()).await {
            Some(n_ctx) => config::ContextBudget::from_context_tokens(n_ctx, args.max_rounds),
            None => config::ContextBudget::default_fallback(),
        }
    };

    eprintln!(
        "context: {} tokens ({}) → budget: {} bytes/result, {} lines/read, {} rounds",
        context_budget.context_tokens,
        context_budget.source,
        context_budget.tool_result_bytes,
        context_budget.read_line_limit,
        args.max_rounds,
    );

    // === New session / context management ( ~/.raven-hotel/ ) ===
    // One persistent session per workspace. Supports goal tracking, repo cache,
    // safe discovery, and a compact injection block for the model.
    let mut sess = session::Session::init(&workspace)?;

    // For interactive use we offer the Cursor-style trust prompt.
    // This decides whether we do deep (but safe) indexing of the tree.
    let is_interactive_tui = args.prompt.is_none();
    if is_interactive_tui {
        // ensure_repo_cache will prompt if not yet trusted
        let _ = session::ensure_repo_cache(&mut sess);
    } else if !sess.meta.trusted {
        // Non-interactive: be conservative, do not index giant trees automatically.
        // The agent can still explore with list/read/grep.
        sess.meta.trusted = false;
        sess.save_meta()?;
    }

    // Small one-time status (goes to stderr so it doesn't pollute --prompt output)
    if is_interactive_tui {
        eprintln!(
            "raven session: {} | goal: {} | repo files: {} | trusted: {}",
            sess.id,
            { let g = &sess.meta.current_goal; let end = g.len().min(60); let end = (0..=end).rev().find(|&i| g.is_char_boundary(i)).unwrap_or(0); &g[..end] },
            sess.meta.repo_cache.total_files_considered,
            sess.meta.trusted
        );
    }

    // === Encrypted endpoint vault ===
    let raven_home = dirs_next_home().join(".raven-hotel");
    let keystore_path = raven_home.join("endpoints.json");
    let mut ks = keystore::Keystore::load_or_create(&keystore_path)
        .unwrap_or_else(|e| {
            eprintln!("warning: could not load endpoints.json: {}", e);
            keystore::Keystore::load_or_create(&keystore_path)
                .expect("FATAL: could not create ~/.raven-hotel/endpoints.json (check permissions)")
        });

    // If any endpoints have encrypted keys, prompt for vault password
    // Supports RAVEN_VAULT_PASSWORD env var for non-interactive / testing use
    if ks.has_encrypted_keys() {
        if let Ok(pw) = std::env::var("RAVEN_VAULT_PASSWORD") {
            match ks.unlock(&pw) {
                Ok(()) => eprintln!("Vault unlocked (from RAVEN_VAULT_PASSWORD)."),
                Err(e) => eprintln!("RAVEN_VAULT_PASSWORD failed: {}. Encrypted endpoints unavailable.", e),
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

    // Decrypt saved endpoints into runtime form
    let saved_endpoints = ks.decrypt_all_endpoints().unwrap_or_default();

    if let Some(prompt) = args.prompt {
        // Non-interactive single shot — still benefits from session (meta is updated on disk)
        // Set the request on the bootstrapped sess before moving it into config.
        let _ = sess.set_last_user_request(&prompt);
        let c = config::Config {
            base_url: args.base_url,
            model: args.model,
            api_key,
            workspace: workspace.clone(),
            temperature: args.temperature,
            max_tokens: args.max_tokens,
            max_rounds: args.max_rounds,
            prebuilt_session: Some(sess),
            context_budget: context_budget.clone(),
        };
        let mut app = agent::Agent::new(c);
        let result = app.run_turn(&prompt).await?;
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
    let c = config::Config {
        base_url: args.base_url,
        model: args.model,
        api_key,
        workspace: workspace.clone(),
        temperature: args.temperature,
        max_tokens: args.max_tokens,
        max_rounds: args.max_rounds,
        prebuilt_session: Some(sess),
        context_budget,
    };

    // Interactive TUI: do not print anything to stdout before entering alternate screen.
    // Trust prompt + any eprintln above already happened on the normal terminal.
    tui_app::run(c, saved_endpoints, ks).await
}

/// Get home directory (simple fallback).
fn dirs_next_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}
