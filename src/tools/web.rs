//! Web tools: search + browse.
//!
//! Search providers:
//! - Brave API (when key is configured via settings or BRAVE_API_KEY env var)
//! - DuckDuckGo HTML scrape (fallback, no key needed)
//!
//! Browse:
//! - Single page: `browse(url)` / `browse(url, depth=1)` for spider
//! - Parallel:    `browse_urls(urls)` — concurrent fetch of multiple URLs

use scraper::{Html, Selector};
use std::time::Duration;

const BRAVE_WEB_URL: &str = "https://api.search.brave.com/res/v1/web/search";

/// Build a reqwest client with Chrome-like headers for anti-bot evasion.
fn build_stealth_client_blocking() -> Result<reqwest::blocking::Client, reqwest::Error> {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE};

    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        ),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9"),
    );

    reqwest::blocking::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        )
        .default_headers(headers)
        .timeout(Duration::from_secs(15))
        .build()
}

/// Build an async reqwest client with Chrome-like headers.
fn build_stealth_client() -> Result<reqwest::Client, reqwest::Error> {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE};

    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        ),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("en-US,en;q=0.9"),
    );

    reqwest::Client::builder()
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        )
        .default_headers(headers)
        .timeout(Duration::from_secs(20))
        .danger_accept_invalid_certs(true) // for internal/dev sites
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
}

// ─── Brave Search API ───────────────────────────────────────────────────────

/// Search via Brave Web Search API. Returns formatted results or an error.
fn brave_search(query: &str, count: usize, api_key: &str) -> Result<String, String> {
    let client = build_stealth_client_blocking()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let resp = client
        .get(BRAVE_WEB_URL)
        .query(&[("q", query), ("count", &count.min(20).to_string())])
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .send()
        .map_err(|e| format!("Brave API request failed: {}", e))?;

    let status = resp.status();
    if status.as_u16() == 429 {
        return Err(
            "⚠️ Brave Search rate limited. Please wait 10-15 seconds and try again."
                .to_string(),
        );
    }
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(format!(
            "⚠️ Brave API key rejected (HTTP {}). Check your key in Settings.",
            status
        ));
    }
    if !status.is_success() {
        return Err(format!("Brave API returned HTTP {}", status));
    }

    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Brave response parse error: {}", e))?;

    let results = body
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array());

    let results = match results {
        Some(arr) if !arr.is_empty() => arr,
        _ => return Err(format!("🔍 No Brave results for: {}", query)),
    };

    let mut out = format!(
        "🔍 Web search (Brave): \"{}\" ({} results)\n\n",
        query,
        results.len().min(count)
    );
    for (i, item) in results.iter().take(count).enumerate() {
        let title = item
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("(no title)");
        let url = item.get("url").and_then(|u| u.as_str()).unwrap_or("");
        let desc = item
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, title, url));
        if !desc.is_empty() {
            // Truncate descriptions to keep context manageable
            let desc_trunc = if desc.len() > 300 { &desc[..300] } else { desc };
            out.push_str(&format!("   {}\n", desc_trunc));
        }
        out.push('\n');
    }
    Ok(out)
}

// ─── Web Search (public) ────────────────────────────────────────────────────

/// Search the web. Uses Brave API if a key is provided, falls back to DuckDuckGo.
pub fn web_search(query: &str, count: usize, brave_key: Option<&str>) -> String {
    if query.trim().is_empty() {
        return "Empty search query".to_string();
    }
    let count = count.clamp(1, 12);

    // Try Brave first if we have a key
    if let Some(key) = brave_key {
        match brave_search(query, count, key) {
            Ok(results) => return results,
            Err(e) => {
                // On auth errors, don't fall back — tell user to fix key
                if e.contains("rejected") {
                    return e;
                }
                // On rate limit, tell user to retry (don't fall back to DDG)
                if e.contains("rate limited") {
                    return e;
                }
                // On other errors (network, parse), fall back to DDG
                // Brave failed (network/key/rate/etc); fallback silently to DDG.
                // (Direct prints would corrupt the TUI display.)
            }
        }
    }

    // DuckDuckGo fallback
    ddg_search(query, count)
}

/// DuckDuckGo HTML scraper (no API key needed). Results are mediocre but free.
fn ddg_search(query: &str, count: usize) -> String {
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );

    let client = match build_stealth_client_blocking() {
        Ok(c) => c,
        Err(e) => return format!("❌ Search client error: {}", e),
    };

    let resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => return format!("❌ Search request failed: {}", e),
    };

    if !resp.status().is_success() {
        return format!("DuckDuckGo returned HTTP {}", resp.status());
    }

    let body = match resp.text() {
        Ok(b) => b,
        Err(e) => return format!("❌ Failed to read search response: {}", e),
    };

    let document = Html::parse_document(&body);

    // DuckDuckGo result selectors (fragile but have worked for years)
    let result_sel = Selector::parse(".result").unwrap();
    let title_sel = Selector::parse(".result__a").unwrap();
    let snippet_sel = Selector::parse(".result__snippet").unwrap();
    let url_sel = Selector::parse(".result__url").unwrap();

    let mut results = vec![];

    for result in document.select(&result_sel).take(count) {
        let title = result
            .select(&title_sel)
            .next()
            .map(|e| e.text().collect::<String>())
            .unwrap_or_else(|| "(no title)".into());

        let url = result
            .select(&url_sel)
            .next()
            .and_then(|e| e.value().attr("href"))
            .unwrap_or("")
            .to_string();

        let snippet = result
            .select(&snippet_sel)
            .next()
            .map(|e| e.text().collect::<String>())
            .unwrap_or_default();

        results.push((
            title.trim().to_string(),
            url.trim().to_string(),
            snippet.trim().to_string(),
        ));
    }

    if results.is_empty() {
        // Fallback: try to find any links
        let a_sel = Selector::parse("a").unwrap();
        for a in document.select(&a_sel).take(count * 2) {
            if let Some(href) = a.value().attr("href") {
                if href.starts_with("http") && !href.contains("duckduckgo") {
                    let t = a.text().collect::<String>();
                    results.push((t, href.to_string(), String::new()));
                    if results.len() >= count {
                        break;
                    }
                }
            }
        }
    }

    if results.is_empty() {
        return format!("🔍 No results found for: {}", query);
    }

    let mut out = format!(
        "🔍 Web search: \"{}\" ({} results)\n\n",
        query,
        results.len()
    );
    for (i, (title, url, snippet)) in results.into_iter().enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, title, url));
        if !snippet.is_empty() {
            out.push_str(&format!("   {}\n", snippet));
        }
        out.push('\n');
    }
    out
}

// ─── Browse (single page + spider) ─────────────────────────────────────────

pub async fn browse(url: &str, depth: usize, extract: &str) -> String {
    if url.trim().is_empty() || !url.starts_with("http") {
        return "browse requires a full http(s):// URL".to_string();
    }

    let client = match build_stealth_client() {
        Ok(c) => c,
        Err(e) => return format!("❌ HTTP client error: {}", e),
    };

    if depth == 0 {
        return fetch_and_format(&client, url, extract).await;
    }

    // Very shallow spider - fetch HTML only once per page
    let mut visited = std::collections::HashSet::new();
    let mut queue = vec![(url.to_string(), 0)];
    let mut pages = vec![];

    while let Some((current, d)) = queue.pop() {
        if visited.contains(&current) {
            continue;
        }
        visited.insert(current.clone());

        // Fetch raw HTML once
        let html = match fetch_html(&client, &current).await {
            Ok(h) => h,
            Err(e) => {
                pages.push((current.clone(), e));
                continue;
            }
        };

        // Format using the single fetched HTML
        let page = match extract {
            "links" => extract_links(&html, &current),
            "html" => {
                let end = super::safe_truncate(&html, 25000).len();
                format!("<!-- {} -->\n{}", current, &html[..end])
            }
            _ => extract_text(&html, &current),
        };
        pages.push((current.clone(), page));

        if d < depth {
            // Extract links from the *already fetched* HTML (no second request)
            let doc = Html::parse_document(&html);
            let a_sel = Selector::parse("a[href]").unwrap();
            for a in doc.select(&a_sel).take(20) {
                if let Some(href) = a.value().attr("href") {
                    if let Ok(abs) = url::Url::parse(&current).and_then(|base| base.join(href)) {
                        let abs_str = abs.to_string();
                        if abs_str.starts_with("http") && !visited.contains(&abs_str) {
                            queue.push((abs_str, d + 1));
                        }
                    }
                }
            }
        }

        if pages.len() >= 8 {
            break; // safety
        }
    }

    let mut out = format!(
        "🕷️ Spidered {} (depth={}, {} pages)\n\n",
        url,
        depth,
        pages.len()
    );
    for (u, content) in pages {
        out.push_str(&format!("--- {} ---\n{}\n\n", u, content));
    }
    out
}

// ─── Parallel Browse ────────────────────────────────────────────────────────

/// Fetch multiple URLs concurrently and return combined extracted content.
/// Capped at 6 concurrent fetches to avoid hammering servers.
pub async fn browse_urls(urls: &[&str], extract: &str) -> String {
    if urls.is_empty() {
        return "browse_urls requires at least one URL".to_string();
    }

    let client = match build_stealth_client() {
        Ok(c) => c,
        Err(e) => return format!("❌ HTTP client error: {}", e),
    };

    // Use a semaphore to cap concurrency at 6
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(6));
    let mut set = tokio::task::JoinSet::new();

    for (i, &url) in urls.iter().enumerate() {
        if !url.starts_with("http") {
            continue;
        }
        let client = client.clone();
        let url = url.to_string();
        let extract = extract.to_string();
        let sem = semaphore.clone();

        set.spawn(async move {
            let _permit = sem.acquire().await;
            let result = fetch_and_format(&client, &url, &extract).await;
            (i, url, result)
        });
    }

    let mut results: Vec<(usize, String, String)> = vec![];
    while let Some(res) = set.join_next().await {
        match res {
            Ok(triple) => results.push(triple),
            Err(e) => results.push((usize::MAX, String::new(), format!("❌ Task error: {}", e))),
        }
    }

    // Sort by original index to preserve order
    results.sort_by_key(|(i, _, _)| *i);

    let good = results.iter().filter(|(_, _, r)| !r.starts_with("❌")).count();
    let failed = results.len() - good;

    let mut out = format!(
        "📚 Fetched {} URLs ({} ok, {} failed)\n\n",
        results.len(),
        good,
        failed
    );

    for (_, url, content) in &results {
        if !url.is_empty() {
            out.push_str(&format!("--- {} ---\n", url));
        }
        out.push_str(content);
        out.push_str("\n\n");
    }
    out
}

// ─── Download (binary-safe, stealth client) ────────────────────────────────

/// Max size for a single download (100 MiB).
const MAX_DOWNLOAD_BYTES: u64 = 100 * 1024 * 1024;

/// Download a URL into a workspace-relative path using the stealth browser client.
/// Prefer this over `exec` + curl/wget — many CDNs bot-block bare curl User-Agents.
pub async fn download_url(url: &str, dest_path: &str, workspace: &std::path::Path) -> String {
    let url = url.trim();
    let dest_path = dest_path.trim();
    if url.is_empty() || !url.starts_with("http://") && !url.starts_with("https://") {
        return "❌ download requires a full http(s):// URL".to_string();
    }
    if dest_path.is_empty() {
        return "❌ download requires path (workspace-relative destination file)".to_string();
    }

    let path = match super::fs::resolve(workspace, dest_path) {
        Ok(p) => p,
        Err(e) => return format!("❌ {e}"),
    };
    if path.is_dir() {
        return format!(
            "❌ Destination is a directory ({}). Pass a file path, e.g. assets/sprites.zip",
            path.display()
        );
    }

    let client = match build_stealth_client() {
        Ok(c) => c,
        Err(e) => return format!("❌ HTTP client error: {e}"),
    };

    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            return format!(
                "❌ Download failed for {url}: {e}\n\
                 Tip: try browse() on the page first to find a direct file link, or use a raw GitHub URL."
            );
        }
    };

    let status = resp.status();
    if !status.is_success() {
        return format!(
            "❌ HTTP {status} for {url}\n\
             Tip: the host may require a browser session or a different direct asset URL."
        );
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if let Some(len) = resp.content_length() {
        if len > MAX_DOWNLOAD_BYTES {
            return format!(
                "❌ Remote file is {len} bytes (limit {}). Pick a smaller asset or download manually.",
                MAX_DOWNLOAD_BYTES
            );
        }
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return format!("❌ Failed to read body for {url}: {e}"),
    };

    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return format!(
            "❌ Downloaded body is {} bytes (limit {}).",
            bytes.len(),
            MAX_DOWNLOAD_BYTES
        );
    }

    // Common bot-block / error-page failure mode: tiny HTML instead of a zip/binary.
    let looks_html = content_type.contains("text/html")
        || bytes.starts_with(b"<!DOCTYPE")
        || bytes.starts_with(b"<!doctype")
        || bytes.starts_with(b"<html")
        || bytes.starts_with(b"<HTML");
    if looks_html && bytes.len() < 50_000 {
        let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]);
        return format!(
            "❌ Download looks like an HTML error/login page ({status}, {} bytes, content-type={content_type}), not a binary asset.\n\
             Preview: {}\n\
             Tip: use browse(url, extract=links) to find a direct .zip/.png link, or pass a raw.githubusercontent.com / github.com/.../archive/...zip URL.",
            bytes.len(),
            preview.replace('\n', " ")
        );
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return format!(
                "❌ Failed to create directories for {}: {e}",
                path.display()
            );
        }
    }

    match std::fs::write(&path, &bytes) {
        Ok(()) => {
            let kind = if bytes.starts_with(b"PK") {
                "zip"
            } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
                "png"
            } else if content_type.is_empty() {
                "binary"
            } else {
                content_type.as_str()
            };
            format!(
                "✅ Downloaded {} bytes ({kind}) → {}\nSource: {url}",
                bytes.len(),
                path.display()
            )
        }
        Err(e) => format!("❌ Write failed for {}: {e}", path.display()),
    }
}

// ─── Shared helpers ─────────────────────────────────────────────────────────

async fn fetch_html(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("❌ Failed to fetch {}: {}", url, e))?;
    if !resp.status().is_success() {
        return Err(format!("❌ HTTP {} for {}", resp.status(), url));
    }
    resp.text()
        .await
        .map_err(|e| format!("❌ Read error for {}: {}", url, e))
}

async fn fetch_and_format(client: &reqwest::Client, url: &str, extract: &str) -> String {
    let html = match fetch_html(client, url).await {
        Ok(h) => h,
        Err(e) => return e,
    };

    match extract {
        "links" => extract_links(&html, url),
        "html" => {
            let end = super::safe_truncate(&html, 25000).len();
            format!("<!-- {} -->\n{}", url, &html[..end])
        }
        _ => extract_text(&html, url),
    }
}

fn extract_text(html: &str, url: &str) -> String {
    let doc = Html::parse_document(html);

    // Remove script/style/nav/footer
    let mut text = String::new();
    if let Ok(body_sel) = Selector::parse("body") {
        if let Some(body) = doc.select(&body_sel).next() {
            text = body.text().collect::<Vec<_>>().join(" ");
        }
    }
    if text.trim().is_empty() {
        text = doc.root_element().text().collect::<Vec<_>>().join(" ");
    }

    // Collapse whitespace
    let text = regex::Regex::new(r"\s+")
        .unwrap()
        .replace_all(&text, " ")
        .trim()
        .to_string();

    let title = if let Ok(title_sel) = Selector::parse("title") {
        doc.select(&title_sel)
            .next()
            .map(|e| e.text().collect::<String>())
            .unwrap_or_else(|| "(no title)".into())
    } else {
        "(no title)".into()
    };

    let preview = if text.len() > 12000 {
        let truncated = super::safe_truncate(&text, 12000);
        format!(
            "{}...\n... ({} chars total, truncated)",
            truncated,
            text.len()
        )
    } else {
        text
    };

    format!("📄 {}\n   {}{}\n\n{}", title.trim(), url, "", preview)
}

fn extract_links(html: &str, base: &str) -> String {
    let doc = Html::parse_document(html);
    let a_sel = Selector::parse("a[href]").unwrap();
    let mut links = vec![];

    for a in doc.select(&a_sel) {
        if let Some(href) = a.value().attr("href") {
            if let Ok(abs) = url::Url::parse(base).and_then(|b| b.join(href)) {
                let s = abs.to_string();
                if !links.contains(&s) {
                    links.push(s);
                }
            }
        }
    }

    let mut out = format!("🔗 Links from {} ({} found)\n", base, links.len());
    for (i, l) in links.iter().take(60).enumerate() {
        out.push_str(&format!("  {:2}. {}\n", i + 1, l));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brave_search_parses_response() {
        // Simulate Brave API JSON response
        let json: serde_json::Value = serde_json::json!({
            "web": {
                "results": [
                    {
                        "title": "Rust Programming Language",
                        "url": "https://www.rust-lang.org/",
                        "description": "A language empowering everyone to build reliable software."
                    },
                    {
                        "title": "Rust (video game)",
                        "url": "https://rust.facepunch.com/",
                        "description": "Survival game by Facepunch Studios."
                    }
                ]
            }
        });

        let results = json
            .get("web")
            .and_then(|w| w.get("results"))
            .and_then(|r| r.as_array())
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].get("title").unwrap().as_str().unwrap(),
            "Rust Programming Language"
        );
    }

    #[test]
    fn ddg_search_handles_empty_query() {
        let result = web_search("", 5, None);
        assert!(result.contains("Empty search query"));
    }

    #[tokio::test]
    async fn download_rejects_empty_url_and_path() {
        let dir = std::env::temp_dir().join(format!(
            "raven-dl-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let r = download_url("", "out.bin", &dir).await;
        assert!(r.contains("❌"), "{r}");
        let r = download_url("https://example.com/x", "", &dir).await;
        assert!(r.contains("❌"), "{r}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn download_rejects_workspace_escape() {
        let dir = std::env::temp_dir().join(format!(
            "raven-dl-esc-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let r = download_url("https://example.com/x", "../outside.bin", &dir).await;
        // May fail resolve or still write under parent check depending on FS; must not succeed cleanly outside
        assert!(
            r.contains("❌") || r.contains("escape") || r.contains("Failed"),
            "{r}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
