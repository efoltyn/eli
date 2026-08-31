//! Citation verification and the citation network — the anti-hallucination tool.
//!
//! Why this exists when case law is on the open web: a search engine will
//! happily surface a page that *contains* a made-up citation, and a language
//! model will happily produce one. Nothing on the open web answers the actual
//! question — *does `999 U.S. 999` name a real decision, and is it the decision
//! you said it was?* This module does, three ways:
//!   * **Existence.** CourtListener's `/c/<reporter>/<volume>/<page>/` resolver
//!     302s to a real opinion and 404s on a reporter page that holds nothing.
//!     No key, and it is outside the API throttle, which matters because
//!     checking a whole brief is exactly the high-volume case.
//!   * **Attribution.** The 302 target carries the real case name. A citation
//!     that exists but resolves to a different case is the failure mode that
//!     survives a spell-check and kills a filing.
//!   * **Ambiguity and normalisation.** With a token, `/citation-lookup/` adds
//!     eyecite's parser: `576 US 644` normalises to `576 U.S. 644`, and
//!     `1 H. 150` comes back three-ways ambiguous rather than confidently wrong.
//!
//! Plus the citation graph — who has relied on a case since (`cited_by`) and
//! what it rested on (`authorities`) — which is Shepard's/KeyCite-shaped data
//! that no consumer search engine exposes.
//!
//! The honest limit, stated in every negative verdict: a 404 means *this
//! corpus* has nothing at that reporter page. That is strong evidence of
//! fabrication, not proof. Unpublished, sealed, very recent, and never-digitised
//! decisions exist outside it.

use crate::legal::courtlistener::{self, CL_WEB};
use crate::legal::{keys, shared_client, strip_markup};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use std::time::Duration;

/// Ceiling on citations verified from one blob of `--text`. Each one costs a
/// request against a shared-IP throttle; a brief with more cites than this
/// should be checked in chunks, and we say so rather than silently truncating.
const MAX_TEXT_CITES: usize = 30;

/// Spacing between key-free resolver calls. Thirty citations at once would
/// otherwise arrive as a burst and read as abuse from a shared egress address.
const RESOLVER_DELAY: Duration = Duration::from_millis(150);

/// The wording every negative verdict carries. Kept in one place because the
/// exact scope of the claim is the whole point: absence from this corpus is
/// evidence, not proof.
const NOT_FOUND_NOTE: &str = "CourtListener has no case at this reporter page. That is strong \
    evidence the citation is fabricated or mis-transcribed, but it is not proof — unpublished \
    dispositions, sealed cases, very recent decisions and never-digitised reporters all exist \
    outside this corpus. Check the reporter, volume and page before concluding the cite is fake.";

#[derive(Clone, Debug)]
pub struct CitationRequest {
    pub text: Option<String>,
    pub cite: Option<String>,
    pub opinion_id: Option<u64>,
    pub cited_by: bool,
    pub authorities: bool,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CitationVerdict {
    pub citation: String,
    pub normalized: Option<String>,
    pub exists: bool,
    /// HTTP-shaped, matching `/citation-lookup/`'s own per-citation vocabulary:
    /// 200 found, 404 no such case, 400 unrecognised reporter, 300 ambiguous,
    /// 429 not checked (throttled or over the batch cap), 0 unreachable.
    pub status: u16,
    pub case_name: Option<String>,
    pub court: Option<String>,
    pub date_filed: Option<String>,
    pub cluster_id: Option<u64>,
    pub url: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct NetworkCase {
    pub case_name: Option<String>,
    pub citation: Option<String>,
    pub court_id: Option<String>,
    pub date_filed: Option<String>,
    /// How many times the citing opinion references the cited one. A depth of 4
    /// is load-bearing authority; a depth of 1 is often a string cite. Only the
    /// token path reports it.
    pub depth: Option<u32>,
    pub opinion_id: Option<u64>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CitationResponse {
    pub generated_at: DateTime<Utc>,
    pub checked: usize,
    pub verified: usize,
    pub unverified: usize,
    pub citations: Vec<CitationVerdict>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cited_by: Vec<NetworkCase>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authorities: Vec<NetworkCase>,
    pub cited_by_total: Option<u64>,
    pub warnings: Vec<String>,
}

pub async fn fetch_citations(req: CitationRequest) -> Result<CitationResponse> {
    let mut out = CitationResponse {
        generated_at: Utc::now(),
        checked: 0,
        verified: 0,
        unverified: 0,
        citations: Vec::new(),
        cited_by: Vec::new(),
        authorities: Vec::new(),
        cited_by_total: None,
        warnings: Vec::new(),
    };

    let text = req.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let single = req.cite.as_deref().map(str::trim).filter(|c| !c.is_empty());
    if text.is_none() && single.is_none() && req.opinion_id.is_none() {
        return Err(Error::InvalidInput(
            "legal cite requires --text (scan a passage), --cite (verify one citation), or --id \
             (walk the citation network)"
                .into(),
        ));
    }

    // What to verify. A single --cite is taken verbatim so the caller sees the
    // verdict on the string they actually wrote, typos included.
    let mut targets: Vec<String> = Vec::new();
    if let Some(c) = single {
        targets.push(c.to_string());
    }
    if let Some(t) = text {
        if t.chars().count() > 64_000 {
            out.warnings.push(
                "--text is longer than the 64,000-character limit the citation API accepts; only \
                 the citations found in the leading portion were checked"
                    .to_string(),
            );
        }
        let found = extract_citations(t);
        if found.is_empty() {
            out.warnings.push(
                "no reporter citations found in --text. This finds the \"<volume> <reporter> \
                 <page>\" shape only — it does not resolve id., supra, statutes or law-journal \
                 cites."
                    .to_string(),
            );
        }
        if found.len() > MAX_TEXT_CITES {
            out.warnings.push(format!(
                "found {} citations; verifying the first {MAX_TEXT_CITES}. Split the passage and \
                 re-run to check the rest.",
                found.len()
            ));
        }
        for c in found.into_iter().take(MAX_TEXT_CITES) {
            if !targets.iter().any(|t| t.eq_ignore_ascii_case(&c)) {
                targets.push(c);
            }
        }
    }

    if !targets.is_empty() {
        out.citations = verify(&targets, text, &mut out.warnings).await;
        out.checked = out.citations.len();
        out.verified = out.citations.iter().filter(|v| v.exists).count();
        out.unverified = out.checked - out.verified;
    }

    if req.cited_by || req.authorities {
        network(&req, &mut out).await;
    }
    Ok(out)
}

// ── verification ───────────────────────────────────────────────────────────

/// Verify a batch: the token-only `/citation-lookup/` first (authoritative case
/// names, normalisation, ambiguity detection, parallel cites), then the key-free
/// resolver for anything it did not cover. The resolver alone is a complete
/// answer — the token is an upgrade, never a prerequisite.
async fn verify(
    targets: &[String],
    text: Option<&str>,
    warnings: &mut Vec<String>,
) -> Vec<CitationVerdict> {
    let mut verdicts: Vec<CitationVerdict> = Vec::new();

    if courtlistener::has_token() {
        // Posting the original passage rather than the extracted strings lets
        // eyecite do its own parsing, which catches forms this module's regex
        // deliberately does not chase.
        let payload = text.map(str::to_string).unwrap_or_else(|| targets.join("; "));
        if let Some(results) = citation_lookup(&payload, warnings).await {
            for r in results {
                verdicts.push(verdict_from_lookup(&r));
            }
        }
    } else {
        warnings.push(
            "no COURTLISTENER_TOKEN: verified with the key-free /c/ resolver. A free token \
             (courtlistener.com/profile/api-token/) adds the /citation-lookup/ endpoint, which \
             returns the authoritative case name, normalises non-standard forms, and flags \
             ambiguous reporter abbreviations."
                .to_string(),
        );
    }

    // Anything the lookup did not answer for — every citation when there is no
    // token — goes through the resolver, one at a time with a pause.
    let mut first = verdicts.is_empty();
    for target in targets {
        if verdicts.iter().any(|v| matches_citation(v, target)) {
            continue;
        }
        if !first {
            tokio::time::sleep(RESOLVER_DELAY).await;
        }
        first = false;
        verdicts.push(resolve_one(target).await);
    }
    verdicts
}

/// Key-free verification of one citation string.
async fn resolve_one(raw: &str) -> CitationVerdict {
    let mut v = CitationVerdict {
        citation: raw.to_string(),
        ..Default::default()
    };
    let Some(parsed) = courtlistener::parse_cite(raw) else {
        v.status = 400;
        v.note = Some(
            "could not be read as \"<volume> <reporter> <page>\", so it was not checked against \
             any corpus"
                .to_string(),
        );
        return v;
    };
    v.normalized = Some(format!(
        "{} {} {}",
        parsed.volume, parsed.reporter, parsed.page
    ));

    let (status, location) = courtlistener::resolve_citation(&parsed).await;
    match status {
        301..=308 => {
            let loc = location.unwrap_or_default();
            let (id, name) = courtlistener::parse_opinion_url(&loc);
            v.exists = true;
            v.status = 200;
            // The redirect lands on the *cluster* page: web URLs on
            // CourtListener are cluster-scoped, and cluster ids and opinion ids
            // are different id spaces.
            v.cluster_id = id;
            v.case_name = name;
            v.url = Some(absolute(&loc));
            v.note = Some(
                "the case name here is read from the URL slug of the page this citation resolves \
                 to. Confirm it matches the case name in your text — a citation that exists but \
                 names a different decision is the error this check is for."
                    .to_string(),
            );
        }
        404 => {
            v.status = 404;
            v.note = Some(NOT_FOUND_NOTE.to_string());
        }
        0 => {
            v.status = 0;
            v.note =
                Some("the citation resolver was unreachable; this cite was NOT checked".to_string());
        }
        429 => {
            v.status = 429;
            v.note = Some(
                "rate limited before this cite could be checked — treat it as unverified, not as \
                 verified or fake"
                    .to_string(),
            );
        }
        other => {
            v.status = other;
            v.note = Some(format!(
                "the citation resolver returned HTTP {other}; this cite was NOT checked"
            ));
        }
    }
    v
}

/// `POST /citation-lookup/` — token only. Form-encoded, and the response is a
/// bare array with one object per citation eyecite found.
async fn citation_lookup(text: &str, warnings: &mut Vec<String>) -> Option<Vec<serde_json::Value>> {
    let token = keys::courtlistener()?;
    // The endpoint parses at most 250 citations per request and blocks payloads
    // over 64,000 characters outright.
    let payload: String = text.chars().take(64_000).collect();
    let resp = shared_client::GENERAL
        .post(format!("{}/citation-lookup/", courtlistener::CL_BASE))
        .header("Authorization", format!("Token {token}"))
        .header("Accept", "application/json")
        .form(&[("text", payload.as_str())])
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("citation-lookup request failed: {e}"));
            return None;
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        warnings.push(match status.as_u16() {
            // The 60/min budget on this endpoint counts *citations*, not
            // requests, and the refusal names the time the budget frees up.
            429 => format!(
                "citation-lookup rate limited (60 valid citations/min); falling back to the \
                 key-free resolver ({snippet})"
            ),
            _ => format!("citation-lookup {status}; falling back to the key-free resolver ({snippet})"),
        });
        return None;
    }
    match resp.json::<Vec<serde_json::Value>>().await {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("citation-lookup parse failed: {e}"));
            None
        }
    }
}

/// Map one `/citation-lookup/` result object onto a verdict.
fn verdict_from_lookup(r: &serde_json::Value) -> CitationVerdict {
    let citation = r
        .get("citation")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let normalized: Vec<String> = r
        .get("normalized_citations")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let status = r.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
    let cluster = r
        .get("clusters")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());

    let mut v = CitationVerdict {
        citation,
        normalized: normalized.first().cloned(),
        exists: status == 200 || status == 300,
        status,
        ..Default::default()
    };
    if let Some(c) = cluster {
        let s = |k: &str| {
            c.get(k)
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_string)
        };
        v.case_name = s("case_name").or_else(|| s("case_name_full"));
        v.date_filed = s("date_filed");
        v.cluster_id = c.get("id").and_then(|x| x.as_u64());
        v.url = s("absolute_url").map(|p| absolute(&p));
    }

    v.note = match status {
        200 => None,
        404 => Some(NOT_FOUND_NOTE.to_string()),
        400 => Some(format!(
            "the reporter abbreviation was not recognised{}. Check the abbreviation against a \
             citation guide; an unrecognised reporter is often an invented one.",
            r.get("error_message")
                .and_then(|x| x.as_str())
                .filter(|m| !m.is_empty())
                .map(|m| format!(" ({m})"))
                .unwrap_or_default()
        )),
        300 => Some(format!(
            "ambiguous — this abbreviation matches more than one reporter. Candidates: {}. \
             Disambiguate before relying on it.",
            normalized.join(", ")
        )),
        429 => Some(
            "beyond the 250-citations-per-request cap: parsed but NOT looked up. Re-run on a \
             shorter passage to check it."
                .to_string(),
        ),
        other => Some(format!("citation-lookup reported status {other}")),
    };
    v
}

/// Did this verdict already answer for `target`? Compares on the normalised
/// digits-and-letters form so "576 US 644" and "576 U.S. 644" are one citation.
fn matches_citation(v: &CitationVerdict, target: &str) -> bool {
    let key = compare_key(target);
    compare_key(&v.citation) == key
        || v.normalized.as_deref().map(compare_key) == Some(key.clone())
}

fn compare_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

// ── citation network ───────────────────────────────────────────────────────

/// Walk the graph around one opinion. `cited_by` (who relies on it since) is
/// available key-free through the search index; `authorities` (what it rested
/// on) is only ids without a token, and we say so rather than returning nothing.
async fn network(req: &CitationRequest, out: &mut CitationResponse) {
    let limit = req.limit.clamp(1, 100);

    // The network is keyed by *opinion* id. A resolved citation gives a cluster
    // id, so when the caller passed a cite rather than an id we take the opinion
    // id off the search hit for that citation.
    let (opinion_id, cites_from_hit) = match req.opinion_id {
        Some(id) => (Some(id), Vec::new()),
        None => {
            let cite = out
                .citations
                .iter()
                .find(|c| c.exists)
                .map(|c| c.normalized.clone().unwrap_or_else(|| c.citation.clone()));
            match cite {
                Some(c) => opinion_id_for_cite(&c, &mut out.warnings).await,
                None => (None, Vec::new()),
            }
        }
    };

    let Some(oid) = opinion_id else {
        out.warnings.push(
            "citation network skipped: no opinion id. Pass --id, or a --cite that resolves to a \
             case in the search index."
                .to_string(),
        );
        return;
    };

    if req.cited_by {
        cited_by(oid, limit, out).await;
    }
    if req.authorities {
        authorities(oid, limit, cites_from_hit, out).await;
    }
}

/// Resolve a citation to an opinion id (and its table of authorities as bare
/// ids) with one anonymous search call.
async fn opinion_id_for_cite(cite: &str, warnings: &mut Vec<String>) -> (Option<u64>, Vec<u64>) {
    let path = format!(
        "search/?q={}&type=o",
        urlencoding::encode(&format!("\"{cite}\""))
    );
    let Some(v) = courtlistener::get(&path, warnings).await else {
        return (None, Vec::new());
    };
    let Some(op) = v
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .and_then(|r| r.get("opinions"))
        .and_then(|o| o.as_array())
        .and_then(|a| a.first())
    else {
        return (None, Vec::new());
    };
    let id = op.get("id").and_then(|x| x.as_u64());
    let cites = op
        .get("cites")
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
        .unwrap_or_default();
    (id, cites)
}

/// Forward citations. `q=cites:(<opinion_id>)` is a fielded query over the same
/// anonymous search index, so this is the one half of the graph that comes back
/// with real case names without a token.
async fn cited_by(opinion_id: u64, limit: usize, out: &mut CitationResponse) {
    let mut path = format!(
        "search/?q={}&type=o&order_by={}",
        urlencoding::encode(&format!("cites:({opinion_id})")),
        urlencoding::encode("citeCount desc")
    );
    if limit > 20 {
        path.push_str(&format!("&page_size={limit}"));
    }
    if let Some(v) = courtlistener::get(&path, &mut out.warnings).await {
        out.cited_by_total = v.get("count").and_then(|c| c.as_u64());
        if let Some(results) = v.get("results").and_then(|r| r.as_array()) {
            for raw in results.iter().take(limit) {
                out.cited_by.push(network_case(raw));
            }
        }
    }

    // With a token the join table gives the exact per-opinion figure; the
    // search count is per-cluster and differs for multi-opinion clusters.
    if courtlistener::has_token() {
        if let Some(v) = courtlistener::get(
            &format!("opinions-cited/?cited_opinion={opinion_id}&count=on"),
            &mut out.warnings,
        )
        .await
        {
            if let Some(n) = v.get("count").and_then(|c| c.as_u64()) {
                out.cited_by_total = Some(n);
            }
        }
    }

    if out.cited_by.is_empty() && out.cited_by_total.unwrap_or(0) == 0 {
        out.warnings.push(format!(
            "nothing found citing opinion {opinion_id}. Either it has not been cited, or the \
             `cites:` field query was rejected — cross-check the case's citeCount on its \
             CourtListener page before reporting it as uncited."
        ));
    }
}

/// Backward citations. The join table is token-gated and, either way, both
/// paths yield opinion *ids*: resolving each to a case name would be one
/// request per authority, which the 5/min throttle makes impossible.
async fn authorities(
    opinion_id: u64,
    limit: usize,
    from_search: Vec<u64>,
    out: &mut CitationResponse,
) {
    if courtlistener::has_token() {
        let path = format!("opinions-cited/?citing_opinion={opinion_id}&page_size={limit}");
        if let Some(v) = courtlistener::get(&path, &mut out.warnings).await {
            if let Some(results) = v.get("results").and_then(|r| r.as_array()) {
                for raw in results.iter().take(limit) {
                    let cited = raw
                        .get("cited_opinion")
                        .and_then(|x| x.as_str())
                        .and_then(last_path_id);
                    out.authorities.push(NetworkCase {
                        depth: raw.get("depth").and_then(|d| d.as_u64()).map(|d| d as u32),
                        opinion_id: cited,
                        url: cited.map(|id| format!("{CL_WEB}/opinion/{id}/")),
                        ..Default::default()
                    });
                }
            }
        }
    }

    if out.authorities.is_empty() {
        for id in from_search.into_iter().take(limit) {
            out.authorities.push(NetworkCase {
                opinion_id: Some(id),
                url: Some(format!("{CL_WEB}/opinion/{id}/")),
                ..Default::default()
            });
        }
    }

    if out.authorities.is_empty() {
        out.warnings.push(format!(
            "no table of authorities for opinion {opinion_id}: /opinions-cited/ is 401 without a \
             token, and the search index returned no `cites` array for this opinion. Set \
             COURTLISTENER_TOKEN to read it."
        ));
    } else if out.authorities.iter().all(|a| a.case_name.is_none()) {
        out.warnings.push(
            "authorities are reported as opinion ids only. Both the join table and the search \
             index give ids, and naming each one costs a request against a 5/min budget — follow \
             the urls, or use the quarterly bulk citation-graph export for real network analysis."
                .to_string(),
        );
    }
}

fn network_case(raw: &serde_json::Value) -> NetworkCase {
    let s = |k: &str| {
        raw.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    NetworkCase {
        case_name: s("caseName").or_else(|| s("caseNameFull")),
        citation: raw
            .get("citation")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.as_str())
            .map(str::to_string),
        court_id: s("court_id"),
        date_filed: s("dateFiled"),
        depth: None,
        opinion_id: raw
            .get("opinions")
            .and_then(|o| o.as_array())
            .and_then(|a| a.first())
            .and_then(|o| o.get("id"))
            .and_then(|x| x.as_u64()),
        url: s("absolute_url").map(|p| absolute(&p)),
    }
}

// ── citation extraction ────────────────────────────────────────────────────

/// The `<volume> <reporter> <page>` shape.
///
/// A reporter token is either capitalised (`U.S.`, `F.`, `Supp.`, `Cal.`) or a
/// bare ordinal (`2d`, `3d`, `4th`) — the ordinal alternative is what lets
/// `F. Supp. 2d` and `Cal. App. 4th` match at all. Volume and page are bounded
/// so a year or a phone number cannot pose as one.
static CITE_RE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?P<vol>\b\d{1,4})\s+(?P<rep>(?:[A-Z][A-Za-z0-9'&.]*|\d[a-z]{1,2}\.?)(?:\s+(?:[A-Z][A-Za-z0-9'&.]*|\d[a-z]{1,2}\.?)){0,4})\s+(?P<page>\d{1,5})\b",
    )
    .ok()
});

/// Capitalised words that start a "<number> <Word> <number>" run without being
/// a reporter. Months matter most: "5 January 2020" is otherwise a perfect
/// structural match.
const NOT_REPORTERS: &[&str] = &[
    "Article", "Section", "Sections", "Chapter", "Part", "Title", "Paragraph", "Page", "Pages",
    "Volume", "Note", "Notes", "Exhibit", "Appendix", "Rule", "Rules", "Table", "Figure", "Item",
    "Line", "Count", "Claim", "Claims", "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
];

/// Reporter forms that are real citations but not *case* citations, so a
/// case-law corpus will always 404 them and the 404 would read as "fabricated".
const NOT_CASE_REPORTERS: &[&str] = &["U.S.C.", "C.F.R.", "Stat.", "Fed. Reg.", "U.S.C.A.", "U.S.C.S."];

/// Pull every citation-looking string out of free text, in order, deduped.
///
/// Deliberately structural rather than clever: it finds the shape and lets the
/// resolver decide what is real. Missing a cite is a smaller harm than
/// pre-filtering away the fabricated one the user needed to catch.
pub(crate) fn extract_citations(text: &str) -> Vec<String> {
    // Markup in a pasted brief would otherwise split a reporter across a tag.
    let flat = if text.contains('<') {
        strip_markup(text)
    } else {
        text.to_string()
    };
    let Some(re) = CITE_RE.as_ref() else {
        return Vec::new();
    };

    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for caps in re.captures_iter(&flat) {
        let (Some(vol), Some(rep), Some(page)) =
            (caps.name("vol"), caps.name("rep"), caps.name("page"))
        else {
            continue;
        };
        let reporter = rep.as_str().trim();
        if !plausible_reporter(reporter) {
            continue;
        }
        let cite = format!("{} {} {}", vol.as_str(), reporter, page.as_str());
        let key = compare_key(&cite);
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(cite);
    }
    out
}

fn plausible_reporter(reporter: &str) -> bool {
    let first = reporter.split_whitespace().next().unwrap_or_default();
    if NOT_REPORTERS.iter().any(|w| w.eq_ignore_ascii_case(first)) {
        return false;
    }
    if NOT_CASE_REPORTERS
        .iter()
        .any(|w| reporter.eq_ignore_ascii_case(w) || reporter.starts_with(w))
    {
        return false;
    }
    // A reporter abbreviation is short and usually punctuated. A single long
    // unpunctuated capitalised word between two numbers is prose ("3 Justices
    // 2"), not a reporter — but keep short bare ones (Wheat, Cranch, Ohio).
    if !reporter.contains('.') && reporter.split_whitespace().count() == 1 && first.len() > 8 {
        return false;
    }
    reporter.len() <= 32
}

// ── shared helpers ─────────────────────────────────────────────────────────

fn absolute(path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else {
        format!("{CL_WEB}/{}", path.trim_start_matches('/'))
    }
}

fn last_path_id(uri: &str) -> Option<u64> {
    uri.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_citations_from_a_paragraph() {
        let text = "See Nken v. Holder, 556 U.S. 418 (2009); Whitfield v. Marshall, \
                    612 F.3d 1099, 1104 (9th Cir. 2010). The court also relied on \
                    Doe v. Roe, 999 U.S. 999 (2021), and on 458 F. Supp. 2d 231 (S.D.N.Y. 2006).";
        let found = extract_citations(text);
        assert_eq!(
            found,
            vec![
                "556 U.S. 418",
                "612 F.3d 1099",
                // The fabricated-looking cite is kept: filtering it here would
                // hide exactly what the caller asked us to check.
                "999 U.S. 999",
                "458 F. Supp. 2d 231",
            ]
        );
    }

    #[test]
    fn a_pincite_does_not_become_a_second_citation() {
        // "612 F.3d 1099, 1104" must yield one cite at page 1099, never a
        // phantom cite at the pincite page.
        let found = extract_citations("612 F.3d 1099, 1104 (9th Cir. 2010)");
        assert_eq!(found, vec!["612 F.3d 1099"]);
    }

    #[test]
    fn duplicate_and_non_canonical_forms_collapse() {
        let found = extract_citations("576 U.S. 644, then again 576 U.S. 644, and 576 US 644.");
        assert_eq!(found, vec!["576 U.S. 644"]);
    }

    #[test]
    fn ignores_dates_headings_and_statutes() {
        assert!(extract_citations("filed 5 January 2020 in this court").is_empty());
        assert!(extract_citations("see 3 Article 7 of the treaty").is_empty());
        assert!(extract_citations("violates 15 U.S.C. 78j and 17 C.F.R. 240").is_empty());
    }

    #[test]
    fn finds_citations_inside_pasted_markup() {
        let html = "<p>See <i>Roe v. Wade</i>, 410 U.S. 113 (1973).</p>";
        assert_eq!(extract_citations(html), vec!["410 U.S. 113"]);
    }

    #[test]
    fn compare_key_ignores_reporter_punctuation() {
        assert_eq!(compare_key("576 U.S. 644"), compare_key("576 US 644"));
        assert_ne!(compare_key("576 U.S. 644"), compare_key("576 U.S. 645"));
    }

    #[test]
    fn lookup_dedupe_matches_on_the_normalized_form() {
        let v = CitationVerdict {
            citation: "576 US 644".into(),
            normalized: Some("576 U.S. 644".into()),
            ..Default::default()
        };
        assert!(matches_citation(&v, "576 U.S. 644"));
        assert!(matches_citation(&v, "576 US 644"));
        assert!(!matches_citation(&v, "410 U.S. 113"));
    }

    #[test]
    fn a_missing_case_is_reported_as_evidence_not_proof() {
        let r = serde_json::json!({
            "citation": "999 U.S. 999",
            "normalized_citations": ["999 U.S. 999"],
            "status": 404,
            "error_message": "Citation not found: '999 U.S. 999'",
            "clusters": []
        });
        let v = verdict_from_lookup(&r);
        assert!(!v.exists);
        assert_eq!(v.status, 404);
        let note = v.note.expect("note");
        assert!(note.contains("not proof"), "{note}");
        assert!(note.contains("unpublished"), "{note}");
    }

    #[test]
    fn ambiguous_lookups_list_their_candidates() {
        let r = serde_json::json!({
            "citation": "1 H. 150",
            "normalized_citations": ["1 Handy 150", "1 Haw. 150", "1 Hill 150"],
            "status": 300,
            "clusters": [{"id": 1, "case_name": "Fell v. Parke", "absolute_url": "/opinion/1/x/"}]
        });
        let v = verdict_from_lookup(&r);
        assert_eq!(v.status, 300);
        let note = v.note.expect("note");
        assert!(note.contains("1 Handy 150"), "{note}");
        assert!(note.contains("1 Hill 150"), "{note}");
        assert_eq!(
            v.url.as_deref(),
            Some("https://www.courtlistener.com/opinion/1/x/")
        );
    }

    #[test]
    fn a_found_citation_carries_the_real_case_name() {
        let r = serde_json::json!({
            "citation": "576 US 644",
            "normalized_citations": ["576 U.S. 644"],
            "status": 200,
            "clusters": [{
                "id": 2812209,
                "case_name": "Obergefell v. Hodges",
                "date_filed": "2015-06-26",
                "absolute_url": "/opinion/2812209/obergefell-v-hodges/"
            }]
        });
        let v = verdict_from_lookup(&r);
        assert!(v.exists);
        assert_eq!(v.case_name.as_deref(), Some("Obergefell v. Hodges"));
        assert_eq!(v.normalized.as_deref(), Some("576 U.S. 644"));
        assert_eq!(v.cluster_id, Some(2812209));
    }

    #[test]
    fn builds_absolute_urls() {
        assert_eq!(
            absolute("/opinion/145884/nken-v-holder/"),
            "https://www.courtlistener.com/opinion/145884/nken-v-holder/"
        );
        assert_eq!(last_path_id("https://x/api/rest/v4/opinions/10008139/"), Some(10008139));
    }

    #[test]
    fn network_case_reads_a_search_hit() {
        let raw = serde_json::json!({
            "caseName": "United States v. Maciel-Alcala",
            "court_id": "ca9",
            "dateFiled": "2010-07-09",
            "citation": ["612 F.3d 1099"],
            "absolute_url": "/opinion/151208/united-states-v-maciel-alcala/",
            "opinions": [{"id": 151208}]
        });
        let n = network_case(&raw);
        assert_eq!(n.case_name.as_deref(), Some("United States v. Maciel-Alcala"));
        assert_eq!(n.citation.as_deref(), Some("612 F.3d 1099"));
        assert_eq!(n.opinion_id, Some(151208));
        assert_eq!(n.court_id.as_deref(), Some("ca9"));
    }
}
