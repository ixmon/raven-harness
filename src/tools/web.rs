//! Web tools: search + browse.
//!
//! We try to stay useful without external API keys:
//! - web_search: Scrapes DuckDuckGo HTML (lightweight, no key). Results are mediocre but better than nothing.
//! - browse: Uses reqwest + scraper for readable text extraction. Supports shallow spidering.
//!
//! In the future you can add a Brave provider (see Raven's web_providers/brave.py) behind a feature flag.

use scraper::{Html, Selector};
use std::time::Duration;

/// Very simple DuckDuckGo scraper. Returns a few results.
pub fn web_search(query: &str, count: usize) -> String {
    if query.trim().is_empty() {
        return "Empty search query".to_string();
    }

    let count = count.clamp(1, 12);
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );

    // Note: this is called via spawn_blocking from tools::execute when used from the async agent loop.
    let client = match reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 RavenTUI/0.1")
        .timeout(Duration::from_secs(15))
        .build()
    {
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

    // DuckDuckGo result selectors (they are a bit fragile but have worked for years)
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

        results.push((title.trim().to_string(), url.trim().to_string(), snippet.trim().to_string()));
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

    let mut out = format!("🔍 Web search: \"{}\" ({} results)\n\n", query, results.len());
    for (i, (title, url, snippet)) in results.into_iter().enumerate() {
        out.push_str(&format!("{}. {}\n   {}\n", i + 1, title, url));
        if !snippet.is_empty() {
            out.push_str(&format!("   {}\n", snippet));
        }
        out.push('\n');
    }
    out
}

pub async fn browse(url: &str, depth: usize, extract: &str) -> String {
    if url.trim().is_empty() || !url.starts_with("http") {
        return "browse requires a full http(s):// URL".to_string();
    }

    let client = match reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 RavenTUI/0.1")
        .timeout(Duration::from_secs(20))
        .danger_accept_invalid_certs(true) // for internal/dev sites
        .build()
    {
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
            "html" => { let end = { let mut e = html.len().min(25000); while e > 0 && !html.is_char_boundary(e) { e -= 1; } e }; format!("<!-- {} -->\n{}", current, &html[..end]) },
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

    let mut out = format!("🕷️ Spidered {} (depth={}, {} pages)\n\n", url, depth, pages.len());
    for (u, content) in pages {
        out.push_str(&format!("--- {} ---\n{}\n\n", u, content));
    }
    out
}

async fn fetch_html(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let resp = client.get(url).send().await.map_err(|e| format!("❌ Failed to fetch {}: {}", url, e))?;
    if !resp.status().is_success() {
        return Err(format!("❌ HTTP {} for {}", resp.status(), url));
    }
    resp.text().await.map_err(|e| format!("❌ Read error for {}: {}", url, e))
}

async fn fetch_and_format(client: &reqwest::Client, url: &str, extract: &str) -> String {
    let html = match fetch_html(client, url).await {
        Ok(h) => h,
        Err(e) => return e,
    };

    match extract {
        "links" => extract_links(&html, url),
        "html" => { let end = { let mut e = html.len().min(25000); while e > 0 && !html.is_char_boundary(e) { e -= 1; } e }; format!("<!-- {} -->\n{}", url, &html[..end]) },
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
    let text = regex::Regex::new(r"\s+").unwrap().replace_all(&text, " ").trim().to_string();

    let title = if let Ok(title_sel) = Selector::parse("title") {
        doc.select(&title_sel)
            .next()
            .map(|e| e.text().collect::<String>())
            .unwrap_or_else(|| "(no title)".into())
    } else {
        "(no title)".into()
    };

    let preview = if text.len() > 12000 {
        let mut end = 12000;
        while end > 0 && !text.is_char_boundary(end) { end -= 1; }
        format!("{}...\n... ({} chars total, truncated)", &text[..end], text.len())
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
