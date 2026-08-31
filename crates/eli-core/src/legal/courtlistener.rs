//! Shared CourtListener plumbing: auth, throttle-aware fetching, and the
//! key-free fallbacks the token-gated endpoints need.
//!
//! Access reality as of this writing, measured rather than assumed:
//!   * Anonymous read works on `/search/`, `/courts/`, `/people/`, `/positions/`,
//!     `/audio/`. Everything else — `/opinions/`, `/clusters/`, `/dockets/`,
//!     `/docket-entries/`, `/recap-documents/`, `/citation-lookup/` — is 401
//!     without a token.
//!   * Throttles are tight and partly IP-keyed: anon 100/day, and the
//!     authenticated buckets (5/min, 50/hour, 125/day) fall back to client IP,
//!     so a shared egress address burns quota you never spent.
//!
//! Everything here therefore treats a token as an upgrade, not a prerequisite,
//! and every token-gated call has a documented key-free path behind it.

use crate::legal::{keys, shared_client};
use serde::Deserialize;

pub(crate) const CL_BASE: &str = "https://www.courtlistener.com/api/rest/v4";
pub(crate) const CL_WEB: &str = "https://www.courtlistener.com";

/// Endpoints that answer without a token. Used to give an honest warning
/// before spending a request we know will 401.
pub(crate) fn is_anon_readable(path: &str) -> bool {
    let p = path.trim_start_matches('/');
    ["search", "courts", "people", "positions", "audio"]
        .iter()
        .any(|prefix| p.starts_with(prefix))
}

pub(crate) fn has_token() -> bool {
    keys::courtlistener().is_some()
}

/// GET a CourtListener API path, returning parsed JSON.
///
/// Never returns an error for an upstream refusal — a 401/429 becomes a
/// warning so the caller can fall back to a key-free path and still answer.
pub(crate) async fn get(
    path_and_query: &str,
    warnings: &mut Vec<String>,
) -> Option<serde_json::Value> {
    let url = format!("{CL_BASE}/{}", path_and_query.trim_start_matches('/'));
    let token = keys::courtlistener();

    if token.is_none() && !is_anon_readable(path_and_query) {
        warnings.push(format!(
            "courtlistener /{}: needs a token (free at courtlistener.com/profile/api/, then set \
             COURTLISTENER_TOKEN). Falling back to what is reachable without one.",
            path_and_query.trim_start_matches('/').split('?').next().unwrap_or("")
        ));
        return None;
    }

    let mut builder = shared_client::GENERAL.get(&url);
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Token {t}"));
    }

    let resp = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("courtlistener request failed: {e}"));
            return None;
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        warnings.push(match status.as_u16() {
            401 | 403 => format!(
                "courtlistener {status}: set COURTLISTENER_TOKEN (free) to reach this endpoint \
                 ({snippet})"
            ),
            429 => format!(
                "courtlistener rate limited{}: anon is 100/day and the per-user buckets \
                 (5/min, 50/hour, 125/day) key off client IP, so a shared address exhausts them. \
                 A token gets you your own bucket. ({snippet})",
                retry_after
                    .map(|r| format!(", retry after {r}s"))
                    .unwrap_or_default()
            ),
            _ => format!("courtlistener {status} ({snippet})"),
        });
        return None;
    }

    match resp.json::<serde_json::Value>().await {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("courtlistener parse failed: {e}"));
            None
        }
    }
}

/// A parsed reporter citation: "597 U.S. 1" -> (597, "U.S.", "1").
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct ParsedCite {
    pub volume: String,
    pub reporter: String,
    pub page: String,
}

/// Parse "<volume> <reporter> <page>" out of a citation string.
///
/// Deliberately loose about the reporter: it is whatever sits between the two
/// numbers, because the reporter abbreviation space is huge and half of it is
/// punctuated inconsistently in the wild.
pub(crate) fn parse_cite(input: &str) -> Option<ParsedCite> {
    let cleaned = input.trim();
    let mut parts = cleaned.split_whitespace();
    let volume = parts.next()?.to_string();
    if volume.is_empty() || !volume.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut rest: Vec<&str> = parts.collect();
    // Drop the trailing court/year parenthetical — "(9th Cir. 2010)" ends in a
    // digit-leading token and would otherwise be read as the page number.
    if let Some(paren) = rest.iter().position(|t| t.starts_with('(')) {
        rest.truncate(paren);
    }
    if rest.len() < 2 {
        return None;
    }
    // The page is the FIRST digit-leading token after the reporter. Taking the
    // last one instead reads the pincite in "597 U.S. 1, 24" as the page, which
    // resolves to a different case — a silent wrong answer, the one failure
    // mode this whole tool exists to prevent. Reporter abbreviations that
    // contain digits ("F.3d", "N.Y.2d") still start with a letter, so they
    // never match here.
    let page_idx = rest
        .iter()
        .position(|t| t.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    if page_idx == 0 {
        return None;
    }
    let page: String = rest[page_idx]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if page.is_empty() {
        return None;
    }
    let reporter = rest[..page_idx].join(" ");
    if reporter.is_empty() {
        return None;
    }
    Some(ParsedCite {
        volume,
        reporter,
        page,
    })
}

/// CourtListener's `/c/` resolver wants the reporter lowercased with the
/// punctuation stripped: "F.3d" -> "f3d", "U.S." -> "us", "S. Ct." -> "sct".
pub(crate) fn slug_reporter(reporter: &str) -> String {
    reporter
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The key-free citation resolver: `/c/<reporter>/<volume>/<page>/`.
///
/// This is the anti-hallucination primitive. A real citation 302s to the
/// opinion page; a fabricated one 404s. It costs no token and is not part of
/// the API throttle, which matters because verifying a brief's citations is
/// exactly the high-volume case.
pub(crate) fn resolver_url(cite: &ParsedCite) -> String {
    format!(
        "{CL_WEB}/c/{}/{}/{}/",
        slug_reporter(&cite.reporter),
        cite.volume,
        cite.page
    )
}

/// Follow the resolver and report where it landed.
///
/// Returns `(status, Some(opinion_url))`. The redirect target carries both the
/// opinion id and a slug of the case name, so a verified hit comes back with
/// the real case name attached — which is what catches a citation that exists
/// but was attributed to the wrong case.
pub(crate) async fn resolve_citation(cite: &ParsedCite) -> (u16, Option<String>) {
    let url = resolver_url(cite);
    // no_redirect: the 302 target *is* the answer; following it costs a second
    // request and a 33 KB error page on the miss path.
    let client = reqwest::Client::builder()
        .user_agent(shared_client::user_agent())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build();
    let Ok(client) = client else {
        return (0, None);
    };
    match client.get(&url).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let location = resp
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            (status, location)
        }
        Err(_) => (0, None),
    }
}

/// Turn `/opinion/6480696/new-york-state-rifle-pistol-assn-inc-v-bruen/` into
/// a readable case name and the opinion id.
pub(crate) fn parse_opinion_url(url: &str) -> (Option<u64>, Option<String>) {
    let path = url.trim_end_matches('/');
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let idx = match segments.iter().position(|s| *s == "opinion") {
        Some(i) => i,
        None => return (None, None),
    };
    let id = segments.get(idx + 1).and_then(|s| s.parse::<u64>().ok());
    let name = segments.get(idx + 2).map(|slug| {
        slug.split('-')
            .map(|w| {
                let mut chars = w.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    });
    (id, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_citation_forms() {
        assert_eq!(
            parse_cite("597 U.S. 1"),
            Some(ParsedCite {
                volume: "597".into(),
                reporter: "U.S.".into(),
                page: "1".into()
            })
        );
        assert_eq!(
            parse_cite("612 F.3d 1099 (9th Cir. 2010)"),
            Some(ParsedCite {
                volume: "612".into(),
                reporter: "F.3d".into(),
                page: "1099".into()
            })
        );
        assert_eq!(
            parse_cite("597 U.S. 1, 24 (2022)"),
            Some(ParsedCite {
                volume: "597".into(),
                reporter: "U.S.".into(),
                page: "1".into()
            })
        );
        assert_eq!(
            parse_cite("410 U. S. 113"),
            Some(ParsedCite {
                volume: "410".into(),
                reporter: "U. S.".into(),
                page: "113".into()
            })
        );
    }

    #[test]
    fn rejects_non_citations() {
        assert_eq!(parse_cite("see also supra"), None);
        assert_eq!(parse_cite("597"), None);
        assert_eq!(parse_cite(""), None);
    }

    #[test]
    fn slugs_reporters_for_the_resolver() {
        assert_eq!(slug_reporter("F.3d"), "f3d");
        assert_eq!(slug_reporter("U.S."), "us");
        assert_eq!(slug_reporter("S. Ct."), "sct");
    }

    #[test]
    fn builds_resolver_url() {
        let c = parse_cite("612 F.3d 1099").expect("parse");
        assert_eq!(
            resolver_url(&c),
            "https://www.courtlistener.com/c/f3d/612/1099/"
        );
    }

    #[test]
    fn reads_case_name_out_of_redirect_target() {
        let (id, name) =
            parse_opinion_url("https://www.courtlistener.com/opinion/145884/nken-v-holder/");
        assert_eq!(id, Some(145884));
        assert_eq!(name.as_deref(), Some("Nken V Holder"));
    }

    #[test]
    fn knows_which_endpoints_need_a_token() {
        assert!(is_anon_readable("search/?q=x"));
        assert!(is_anon_readable("courts/"));
        assert!(!is_anon_readable("dockets/?id=1"));
        assert!(!is_anon_readable("citation-lookup/"));
    }
}
