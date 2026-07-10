//! Probe Brave Search the same way the agent `web_search` tool does.
//!
//! Loads the key from `~/.raven-hotel/endpoints.json` (via the real keystore)
//! or `BRAVE_API_KEY`, then calls `raven_tui::tools::web_search`.
//!
//! Usage (from `tui/`):
//! ```text
//! RAVEN_VAULT_PASSWORD=raven cargo run -q --example brave_search_probe -- "Kenney sprites"
//! cargo run -q --example brave_search_probe -- --raw "galaga sdl2"
//! BRAVE_API_KEY=BSA... cargo run -q --example brave_search_probe -- "test query"
//! ```

#[path = "../src/keystore.rs"]
mod keystore;

use keystore::Keystore;
use raven_tui::tools::web_search;
use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

const BRAVE_WEB_URL: &str = "https://api.search.brave.com/res/v1/web/search";

fn raven_home() -> PathBuf {
    env::var_os("RAVEN_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs_fallback_home()
                .map(|h| h.join(".raven-hotel"))
                .unwrap_or_else(|| PathBuf::from(".raven-hotel"))
        })
}

fn dirs_fallback_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn load_brave_key() -> Result<(String, String), String> {
    // Prefer env if set (matches keystore fallback when vault locked, and override for tests).
    if let Ok(k) = env::var("BRAVE_API_KEY") {
        if !k.is_empty() {
            return Ok((k, "BRAVE_API_KEY env".into()));
        }
    }

    let path = raven_home().join("endpoints.json");
    if !path.exists() {
        return Err(format!(
            "no BRAVE_API_KEY and no keystore at {}",
            path.display()
        ));
    }

    let mut ks = Keystore::load_or_create(&path).map_err(|e| e.to_string())?;
    if !ks.has_brave_key() {
        return Err(format!(
            "keystore at {} has no Brave key (and BRAVE_API_KEY unset)",
            path.display()
        ));
    }

    // Encrypted blob needs unlock; env-only has_brave_key is already handled above.
    if ks.get_brave_key().is_none() {
        let password = env::var("RAVEN_VAULT_PASSWORD").unwrap_or_else(|_| {
            eprint!("Vault password: ");
            let _ = io::stderr().flush();
            rpassword::read_password().unwrap_or_default()
        });
        if password.is_empty() {
            return Err(
                "vault has encrypted Brave key but no password (set RAVEN_VAULT_PASSWORD)".into(),
            );
        }
        ks.unlock(&password)
            .map_err(|e| format!("vault unlock failed: {e}"))?;
    }

    match ks.get_brave_key() {
        Some(k) => Ok((k, format!("keystore {}", path.display()))),
        None => Err(
            "Brave key present in keystore but did not decrypt (wrong password?)".into(),
        ),
    }
}

/// Same endpoint/headers as `tools/web.rs::brave_search`, but print status + raw body.
fn raw_brave_request(query: &str, count: usize, api_key: &str) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get(BRAVE_WEB_URL)
        .query(&[("q", query), ("count", &count.min(20).to_string())])
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    let headers_interest: Vec<String> = ["x-ratelimit-limit", "x-rateLimit-remaining", "retry-after"]
        .iter()
        .filter_map(|h| {
            resp.headers()
                .get(*h)
                .and_then(|v| v.to_str().ok())
                .map(|v| format!("{h}: {v}"))
        })
        .collect();

    let body = resp.text().unwrap_or_default();
    println!("--- raw Brave API ---");
    println!("GET {BRAVE_WEB_URL}");
    println!("q={query:?} count={}", count.min(20));
    println!("HTTP {status}");
    if !headers_interest.is_empty() {
        println!("headers: {}", headers_interest.join(", "));
    }
    let preview = if body.len() > 2000 {
        format!("{}… ({} bytes total)", &body[..2000], body.len())
    } else {
        body.clone()
    };
    println!("body:\n{preview}");
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("Brave returned HTTP {status}"))
    }
}

fn print_usage() {
    eprintln!(
        "Usage: brave_search_probe [--raw] [--count N] <query...>\n\n\
         Env:\n\
           RAVEN_VAULT_PASSWORD  unlock ~/.raven-hotel/endpoints.json\n\
           BRAVE_API_KEY         skip vault (optional override)\n\
           RAVEN_HOME            default ~/.raven-hotel\n"
    );
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_usage();
        std::process::exit(if args.is_empty() { 1 } else { 0 });
    }

    let mut raw = false;
    let mut count: usize = 5;
    let mut query_parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--raw" => raw = true,
            "--count" => {
                i += 1;
                count = args
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(5)
                    .clamp(1, 20);
            }
            s if s.starts_with("--count=") => {
                count = s
                    .trim_start_matches("--count=")
                    .parse()
                    .unwrap_or(5)
                    .clamp(1, 20);
            }
            s => query_parts.push(s.to_string()),
        }
        i += 1;
    }
    let query = query_parts.join(" ");
    if query.trim().is_empty() {
        print_usage();
        std::process::exit(1);
    }

    let (key, source) = match load_brave_key() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    // Never print the full key.
    let key_hint = if key.len() > 8 {
        format!("{}…{}", &key[..4], &key[key.len() - 4..])
    } else {
        "(short key)".into()
    };
    println!("key source: {source}");
    println!("key hint:   {key_hint} (len={})", key.len());
    println!("query:      {query:?}");
    println!("count:      {count}");
    println!();

    if raw {
        match raw_brave_request(&query, count, &key) {
            Ok(()) => println!("\nraw request: OK"),
            Err(e) => {
                eprintln!("\nraw request: {e}");
                std::process::exit(3);
            }
        }
        println!();
    }

    println!("--- tools::web_search (same entry as agent tool) ---");
    let out = web_search(&query, count, Some(&key));
    println!("{out}");

    if out.contains("Brave Search failed") || out.contains("⚠️ Brave") {
        std::process::exit(4);
    }
    if out.starts_with("❌") || out.contains("No results found") || out.contains("No Brave results")
    {
        std::process::exit(5);
    }
}
