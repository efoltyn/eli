//! Legal research data sources.
//!
//! Same shape as `finance/`: each submodule owns one upstream, exposes a
//! `*Request` / `*Response` pair and one `fetch_*` entry point. Nothing here
//! renders — the CLI layer serializes the response to JSON and the MCP layer
//! summarizes it.
//!
//! Design rule for this module tree: **degrade, never fail**. Legal upstreams
//! are a mix of key-free (Federal Register, eCFR, govinfo bulk) and key-gated
//! (regulations.gov, CourtListener at useful rate limits). A missing key or a
//! throttled upstream must come back as a `warnings` entry plus whatever data
//! we did get, not as an error — a half-answered docket is still worth more
//! than a hard failure.

use crate::{Error, Result};

/// Shared HTTP clients for legal upstreams.
///
/// Separate from `finance::shared_client` because several of these hosts
/// (SEC, CourtListener, GPO) enforce or request a descriptive User-Agent and
/// will 403 a generic one.
pub(crate) mod shared_client {
    use std::sync::LazyLock;

    /// Identifies the tool to upstreams that ask for it (SEC explicitly
    /// requires a UA with contact info; GPO and CourtListener ask politely).
    /// Override with `LEGAL_SEARCH_USER_AGENT` when running at volume so the
    /// operator, not this project, owns the contact address.
    pub(crate) fn user_agent() -> String {
        std::env::var("LEGAL_SEARCH_USER_AGENT").unwrap_or_else(|_| {
            "legal-search/0.3 (+https://github.com/efoltyn/market-search)".to_string()
        })
    }

    /// Unlike the finance clients these honour the environment's proxy
    /// settings (no `.no_proxy()`). Several of these hosts — federalregister.gov's
    /// full-text endpoints among them — answer a proxied request and 302 a
    /// direct one into an access wall.
    pub(crate) static GENERAL: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .user_agent(user_agent())
            .tcp_nodelay(true)
            .timeout(std::time::Duration::from_secs(45))
            .connect_timeout(std::time::Duration::from_secs(10))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(4)
            .build()
            .expect("failed to build shared legal HTTP client")
    });

    /// Long-timeout client for the bulk XML endpoints (a full eCFR title can
    /// be tens of megabytes and take a while to stream).
    pub(crate) static BULK: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .user_agent(user_agent())
            .tcp_nodelay(true)
            .timeout(std::time::Duration::from_secs(180))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build bulk legal HTTP client")
    });
}

/// Credentials for the key-gated upstreams. All optional: every tool that can
/// answer without a key must still answer without a key, and say in
/// `warnings` what the key would have added.
pub(crate) mod keys {
    /// CourtListener personal API token. Anonymous access exists but is
    /// throttled hard enough to be unusable in practice on a shared egress IP;
    /// a free token raises it to 5,000/hr.
    pub(crate) fn courtlistener() -> Option<String> {
        first_env(&["COURTLISTENER_TOKEN", "COURTLISTENER_API_TOKEN"])
    }

    /// api.data.gov key — one key covers regulations.gov, govinfo and
    /// congress.gov.
    pub(crate) fn data_gov() -> Option<String> {
        first_env(&[
            "REGULATIONS_GOV_API_KEY",
            "DATA_GOV_API_KEY",
            "GOVINFO_API_KEY",
            "API_DATA_GOV_KEY",
        ])
    }

    fn first_env(names: &[&str]) -> Option<String> {
        names.iter().find_map(|n| {
            std::env::var(n)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
    }
}

/// Turn a non-2xx response into a `warnings` line rather than an error, with
/// the upstream body trimmed to something readable. Returns `None` when the
/// caller should skip this source and carry on.
pub(crate) async fn soft_fail(
    source: &str,
    resp: reqwest::Response,
    warnings: &mut Vec<String>,
) -> Option<reqwest::Response> {
    if resp.status().is_success() {
        return Some(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let snippet: String = body.chars().take(240).collect();
    warnings.push(match status.as_u16() {
        401 | 403 => format!("{source}: {status} — upstream requires a key or a contact User-Agent ({snippet})"),
        429 => format!("{source}: rate limited by upstream ({snippet})"),
        404 => format!("{source}: not found ({snippet})"),
        _ => format!("{source}: {status} ({snippet})"),
    });
    None
}

/// Tags that sit *inside* a word. A closing `</strong>` must not become a
/// space, or `Act of 202</strong><strong>2` — which is how CRS summaries and
/// search-hit highlighting arrive — reads back as "Act of 202 2".
const INLINE_TAGS: &[&str] = &[
    "a", "b", "i", "u", "em", "strong", "span", "sup", "sub", "mark", "small", "code", "abbr",
    "cite", "q", "s", "var", "wbr", "e",
];

/// Strip HTML/XML tags and collapse whitespace. Several of these upstreams
/// only publish document bodies as markup; the model wants the words.
///
/// Block-level tags become a space (so `</p><p>` doesn't weld two sentences
/// together); inline tags become nothing (so a highlighted fragment doesn't
/// split a word in half).
pub(crate) fn strip_markup(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut tag = String::new();
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                if !is_inline_tag(&tag) {
                    out.push(' ');
                }
            }
            c if in_tag => tag.push(c),
            c => out.push(c),
        }
    }
    let decoded = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#8217;", "'")
        .replace("&#8220;", "\"")
        .replace("&#8221;", "\"")
        .replace("&nbsp;", " ");
    let mut collapsed = String::with_capacity(decoded.len());
    let mut last_ws = false;
    for ch in decoded.chars() {
        if ch.is_whitespace() {
            if !last_ws {
                collapsed.push(' ');
            }
            last_ws = true;
        } else {
            collapsed.push(ch);
            last_ws = false;
        }
    }
    // Tag boundaries become spaces, which strands one before any punctuation
    // that followed a closing tag ("information ."). Left in, it shows up in
    // quoted regulatory text.
    let mut out = String::with_capacity(collapsed.len());
    for ch in collapsed.chars() {
        if matches!(ch, '.' | ',' | ';' | ':' | ')' | ']' | '!' | '?') && out.ends_with(' ') {
            out.pop();
        }
        out.push(ch);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_entities_and_stranded_punctuation() {
        assert_eq!(
            strip_markup("<p>Misuse of <strong>material</strong> information</p>."),
            "Misuse of material information."
        );
        assert_eq!(strip_markup("A &amp; B &quot;quoted&quot;"), "A & B \"quoted\"");
        assert_eq!(strip_markup("<a href=\"x\">link</a> ; next"), "link; next");
    }

    #[test]
    fn inline_tags_do_not_split_words() {
        // How CRS summaries and search highlighting actually arrive.
        assert_eq!(
            strip_markup("Postal Service Reform Act of 202</strong><strong>2"),
            "Postal Service Reform Act of 2022"
        );
        assert_eq!(
            strip_markup("misuse of <strong>material</strong> information"),
            "misuse of material information"
        );
        // Block tags still separate — otherwise sentences weld together.
        assert_eq!(strip_markup("<p>One.</p><p>Two.</p>"), "One. Two.");
    }

    #[test]
    fn keeps_line_structure_when_asked() {
        let doc = "[FR Doc No: 2025-02524]\nvia the GPO [<a href=\"http://gpo.gov\">gpo.gov</a>]\n\nSUMMARY:";
        assert_eq!(
            strip_tags_keep_lines(doc),
            "[FR Doc No: 2025-02524]\nvia the GPO [gpo.gov]\n\nSUMMARY:"
        );
    }

    #[test]
    fn clamp_text_flags_the_cut() {
        let (text, truncated) = clamp_text("abcdef", 3);
        assert_eq!(text, "abc");
        assert!(truncated);
        let (text, truncated) = clamp_text("abc", 10);
        assert_eq!(text, "abc");
        assert!(!truncated);
    }

    #[test]
    fn parse_date_rejects_other_formats() {
        assert!(parse_date("2024-01-01", "--date").is_ok());
        assert!(parse_date("01/01/2024", "--date").is_err());
        assert!(parse_date("yesterday", "--date").is_err());
    }
}

/// `"/strong"`, `"a href=\"x\""`, `"br/"` -> the bare tag name.
fn is_inline_tag(raw: &str) -> bool {
    let name: String = raw
        .trim()
        .trim_start_matches('/')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    INLINE_TAGS.contains(&name.as_str())
}

/// Strip tags but keep the line structure. Plain-text documents that arrive
/// wrapped in markup (the Federal Register serves rule text inside `<pre>`,
/// with `<a>` tags spliced into it) lose their paragraphing entirely if run
/// through `strip_markup`, which collapses every newline.
pub(crate) fn strip_tags_keep_lines(input: &str) -> String {
    input
        .lines()
        .map(strip_markup)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Clamp a document body so one tool call can't blow the context window.
/// Returns the text plus whether it was cut.
pub(crate) fn clamp_text(text: &str, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

/// Validate a `YYYY-MM-DD` date, which every point-in-time endpoint here needs.
pub(crate) fn parse_date(input: &str, field: &str) -> Result<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(input.trim(), "%Y-%m-%d")
        .map_err(|_| Error::InvalidInput(format!("{field} must be YYYY-MM-DD, got {input:?}")))
}

pub mod caselaw;
pub(crate) mod courtlistener;
pub mod cfr;
pub mod citator;
pub mod comments;
pub mod docket;
pub mod enforcement;
pub mod fedreg;
pub mod statute;
