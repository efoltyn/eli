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

    pub(crate) static GENERAL: LazyLock<reqwest::Client> = LazyLock::new(|| {
        reqwest::Client::builder()
            .user_agent(user_agent())
            .tcp_nodelay(true)
            .timeout(std::time::Duration::from_secs(45))
            .connect_timeout(std::time::Duration::from_secs(10))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .pool_max_idle_per_host(4)
            .no_proxy()
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
            .no_proxy()
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

/// Strip HTML/XML tags and collapse whitespace. Several of these upstreams
/// only publish document bodies as markup; the model wants the words.
pub(crate) fn strip_markup(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
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
    collapsed.trim().to_string()
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
pub mod cfr;
pub mod citator;
pub mod comments;
pub mod docket;
pub mod enforcement;
pub mod fedreg;
pub mod statute;
