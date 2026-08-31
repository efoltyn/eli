//! Wisconsin — statutory text from the Legislature, trial-court records from
//! WCCA/CCAP.
//!
//! Why this exists when both sites are on the open web:
//!
//!   * **The statute you were actually charged under.** A search engine
//!     indexes whatever Wisconsin's site rendered when it last crawled, and
//!     third-party mirrors (Justia, FindLaw, Casetext) lag the biennial
//!     renumbering by months. `docs.legis.wisconsin.gov` is the publisher of
//!     record, and the section number on a citation maps to it directly.
//!
//!   * **Trial-court case records — the layer that essentially does not exist
//!     anywhere else.** Traffic citations, small claims and misdemeanors are
//!     decided in circuit courts, which in nearly every state publish nothing
//!     machine-readable. Wisconsin's WCCA (the public face of CCAP, the state
//!     circuit-court case management system) answers two key-free JSON routes,
//!     and that is the whole reason this module tree has a `trial_records`
//!     column at all. Nothing in CourtListener, PACER or any federal source
//!     reaches down here — those start at the appellate level.
//!
//! Two hard limits are deliberate, not oversights:
//!
//!   * **Result lists only, never case detail.** WCCA's `caseDetail` view is
//!     gated behind a `{tac, captcha}` token pair. This tool does not attempt,
//!     solve, or work around that captcha; it emits the human URL and says so
//!     in `warnings`. WCCA's sanctioned bulk feed is subscription-only and is
//!     likewise not touched.
//!
//!   * **PII hygiene.** These are records about private individuals who mostly
//!     did nothing more interesting than speed. `party_name` is the case
//!     caption and unavoidable; the date of birth is not, so it is dropped
//!     unless the caller explicitly opted in — and dropped regardless when
//!     WCCA flags it sealed.
//!
//! No API key, no cookie, no session on any route used here. Requests are
//! sequential with a fixed delay and a hard window cap: this is a live
//! government service with no rate-limit documentation, so the polite ceiling
//! is self-imposed.

use super::{
    StateCaseRecord, StateCaseRequest, StateCaseResponse, StateStatuteRequest,
    StateStatuteResponse,
};
use crate::legal::{clamp_text, parse_date, shared_client, soft_fail, strip_markup};
use crate::{Error, Result};
use chrono::{Duration, NaiveDate, Utc};
use serde::Deserialize;
use std::sync::LazyLock;

// ── endpoints ──────────────────────────────────────────────────────────────

/// The `/document/statutes/{section}` route is the citable one. It 302s to the
/// physical `/statutes/statutes/{chapter}/{subchapter-roman}/{section}` path,
/// which no caller can construct without already knowing the subchapter's
/// roman numeral — so we always enter through `/document/` and let reqwest
/// follow the redirect.
const STATUTE_BASE: &str = "https://docs.legis.wisconsin.gov/document/statutes";
const STATUTE_SOURCE: &str = "docs.legis.wisconsin.gov (Wisconsin Legislature, Wis. Stats.)";

const WCCA_CASE_SEARCH: &str = "https://wcca.wicourts.gov/jsonPost/caseSearch";
const WCCA_ADVANCED_SEARCH: &str = "https://wcca.wicourts.gov/jsonPost/advancedCaseSearch";
const WCCA_DETAIL_BASE: &str = "https://wcca.wicourts.gov/caseDetail.html";
const WCCA_SOURCE: &str = "wcca.wicourts.gov (Wisconsin Circuit Court Access)";

/// Measured upstream limit: `advancedCaseSearch` rejects a filing-date range
/// wider than 30 days with `errors.filingDate.compositeError`. A caller asking
/// for a quarter is therefore served by several sequential windows rather than
/// one failed call.
const MAX_RANGE_DAYS: i64 = 29;

/// Hard cap on those windows. Six windows is roughly six months of coverage
/// and six requests — past that the caller should narrow the question rather
/// than have this tool walk a state court system's database for them.
const MAX_WINDOWS: usize = 6;

/// Between sequential requests. WCCA publishes no rate limit, so we pick a
/// conservative one instead of discovering theirs.
const POLITE_DELAY_MS: u64 = 300;

/// One 30-day window of Milwaukee traffic cases is ~2,400 rows; the endpoint
/// itself does not paginate or cap. Without a ceiling here a single call could
/// serialize tens of thousands of records about named private individuals.
const MAX_LIMIT: usize = 500;

// ── county numbers ─────────────────────────────────────────────────────────

/// WCCA is county-scoped by number, and a *wrong* number does not error — it
/// silently returns a different county's cases, which is the worst failure
/// mode available here. So this table is not inferred from the obvious
/// alphabetical pattern; every row was read back from the API itself, by
/// issuing a search for each number and recording the `countyName` WCCA
/// returned. Menominee sitting at 72 rather than in alphabetical order (it was
/// created in 1961, after the numbering) is exactly the kind of thing that
/// would have made a guessed table wrong.
const COUNTIES: &[(u32, &str)] = &[
    (1, "Adams"),
    (2, "Ashland"),
    (3, "Barron"),
    (4, "Bayfield"),
    (5, "Brown"),
    (6, "Buffalo"),
    (7, "Burnett"),
    (8, "Calumet"),
    (9, "Chippewa"),
    (10, "Clark"),
    (11, "Columbia"),
    (12, "Crawford"),
    (13, "Dane"),
    (14, "Dodge"),
    (15, "Door"),
    (16, "Douglas"),
    (17, "Dunn"),
    (18, "Eau Claire"),
    (19, "Florence"),
    (20, "Fond du Lac"),
    (21, "Forest"),
    (22, "Grant"),
    (23, "Green"),
    (24, "Green Lake"),
    (25, "Iowa"),
    (26, "Iron"),
    (27, "Jackson"),
    (28, "Jefferson"),
    (29, "Juneau"),
    (30, "Kenosha"),
    (31, "Kewaunee"),
    (32, "La Crosse"),
    (33, "Lafayette"),
    (34, "Langlade"),
    (35, "Lincoln"),
    (36, "Manitowoc"),
    (37, "Marathon"),
    (38, "Marinette"),
    (39, "Marquette"),
    (40, "Milwaukee"),
    (41, "Monroe"),
    (42, "Oconto"),
    (43, "Oneida"),
    (44, "Outagamie"),
    (45, "Ozaukee"),
    (46, "Pepin"),
    (47, "Pierce"),
    (48, "Polk"),
    (49, "Portage"),
    (50, "Price"),
    (51, "Racine"),
    (52, "Richland"),
    (53, "Rock"),
    (54, "Rusk"),
    (55, "St Croix"),
    (56, "Sauk"),
    (57, "Sawyer"),
    (58, "Shawano"),
    (59, "Sheboygan"),
    (60, "Taylor"),
    (61, "Trempealeau"),
    (62, "Vernon"),
    (63, "Vilas"),
    (64, "Walworth"),
    (65, "Washburn"),
    (66, "Washington"),
    (67, "Waukesha"),
    (68, "Waupaca"),
    (69, "Waushara"),
    (70, "Winnebago"),
    (71, "Wood"),
    (72, "Menominee"),
];

// ── statutes ───────────────────────────────────────────────────────────────

pub(super) async fn fetch_statute(req: StateStatuteRequest) -> Result<StateStatuteResponse> {
    let mut warnings: Vec<String> = Vec::new();
    let section = normalize_section(&req.section, req.chapter.as_deref())?;
    let url = format!("{STATUTE_BASE}/{}", urlencoding::encode(&section));

    let mut out = StateStatuteResponse {
        generated_at: Utc::now(),
        state: "wi".to_string(),
        citation: None,
        heading: None,
        text: None,
        chars: 0,
        truncated: false,
        repealed: None,
        source: Some(STATUTE_SOURCE.to_string()),
        source_url: Some(url.clone()),
        warnings: Vec::new(),
    };

    let Some(html) = get_text(&url, "wisconsin statutes", &mut warnings).await else {
        out.warnings = warnings;
        return Ok(out);
    };

    // Status code alone proves nothing on this host. A bad section 404s with a
    // ~12 KB chrome-only page, and — worse — a *valid-looking* section number
    // can redirect to the subchapter page that would have contained it, which
    // returns 200 and 90 KB of the neighbouring sections' text. Only the
    // presence of the section's own blocks proves we got what was asked for.
    let blocks = section_blocks(&html, &section);
    if blocks.is_empty() {
        warnings.push(format!(
            "Wis. Stat. § {section} is not in the current statutes: the page served carries no \
             text for that section. Either the number is wrong or the section has been repealed \
             out of the code entirely (Wisconsin removes repealed sections rather than leaving a \
             stub). Check the chapter's table of contents at {STATUTE_BASE}/{}.",
            urlencoding::encode(chapter_of(&section).as_deref().unwrap_or(&section))
        ));
        out.warnings = warnings;
        return Ok(out);
    }

    out.citation = Some(format!("Wis. Stat. § {section}"));
    // The first block is the section head: "346.57 Speed restrictions." The
    // number is already in `citation`, so strip it rather than say it twice.
    out.heading = blocks
        .first()
        .map(|b| strip_leading_section(b, &section))
        .filter(|h| !h.is_empty());

    // One line per source block. Wisconsin's markup is one <div> per
    // subsection/paragraph and nothing else carries the structure, so a flat
    // strip_markup over the whole page would weld "(1) Definitions" onto the
    // preceding sentence and make subsection boundaries unrecoverable.
    let body = blocks.join("\n");
    out.repealed = Some(looks_repealed(&body));
    if out.repealed == Some(true) {
        warnings.push(format!(
            "Wis. Stat. § {section} carries a repeal note — do not quote it as current law \
             without reading the note in the text."
        ));
    }

    // `chars` is the FULL length by contract, so a caller can see how much of
    // the section it did not get; reporting the clamped length would make it
    // equal max_chars and hide the remainder.
    out.chars = body.chars().count();
    let (text, truncated) = clamp_text(&body, req.max_chars);
    out.truncated = truncated;
    if truncated {
        warnings.push(format!(
            "section text cut to {} of {} chars; raise --max-chars for the rest",
            req.max_chars, out.chars
        ));
    }
    out.text = Some(text);
    out.warnings = warnings;
    Ok(out)
}

// ── trial-court records ────────────────────────────────────────────────────

pub(super) async fn fetch_cases(req: StateCaseRequest) -> Result<StateCaseResponse> {
    let mut warnings: Vec<String> = Vec::new();
    let limit = req.limit.clamp(1, MAX_LIMIT);
    if req.limit > MAX_LIMIT {
        warnings.push(format!(
            "limit capped at {MAX_LIMIT}: these are records naming private individuals, and one \
             30-day county window can exceed 2,000 rows"
        ));
    }

    // County is optional on both routes (verified: caseSearch without
    // countyNo matches the case number across all 72 counties), but an
    // *unrecognised* county name must never fall through to an unfiltered
    // search — the caller would silently get the wrong county's docket.
    let county_no = match req.county.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match county_number(raw) {
            Some(n) => Some(n),
            None => {
                return Err(Error::InvalidInput(format!(
                    "unknown Wisconsin county {raw:?}. WCCA is keyed by county number and a wrong \
                     number silently returns another county's cases, so this is not guessed. \
                     Valid: {}",
                    county_names().join(", ")
                )));
            }
        },
        _ => None,
    };

    let (cases, source_url) = if let Some(case_no) = req.case_no.as_deref() {
        single_case(case_no, county_no, &mut warnings).await?
    } else {
        advanced_search(&req, county_no, limit, &mut warnings).await?
    };

    let sealed = cases.iter().filter(|c| c.is_dob_sealed).count();
    let mut records: Vec<StateCaseRecord> = cases
        .into_iter()
        .map(|c| to_record(c, req.include_dob))
        .collect();
    records.truncate(limit);

    if !records.is_empty() {
        if req.include_dob {
            warnings.push(
                "--include-dob was set: dates of birth are included at WCCA's own \
                 year-month precision. They are omitted for any case WCCA flags sealed."
                    .to_string(),
            );
            if sealed > 0 {
                warnings.push(format!(
                    "{sealed} of the returned cases have a sealed date of birth; those are \
                     omitted regardless of --include-dob"
                ));
            }
        }
        warnings.push(
            "result list only. WCCA's per-case detail view (charges, sentence, hearings) is \
             gated behind a captcha, which this tool deliberately does not attempt — open the \
             per-case `url` in a browser for that."
                .to_string(),
        );
    }

    Ok(StateCaseResponse {
        generated_at: Utc::now(),
        state: "wi".to_string(),
        returned: records.len(),
        cases: records,
        source: Some(WCCA_SOURCE.to_string()),
        source_url: Some(source_url),
        warnings,
    })
}

/// Exact case number. County narrows it; without one WCCA matches the number
/// in every county, which is usually what a caller holding only the number off
/// a citation actually wants.
async fn single_case(
    case_no: &str,
    county_no: Option<u32>,
    warnings: &mut Vec<String>,
) -> Result<(Vec<WccaCase>, String)> {
    let case_no = case_no.trim().to_ascii_uppercase();
    if !looks_like_case_no(&case_no) {
        return Err(Error::InvalidInput(format!(
            "case number {case_no:?} is not a Wisconsin circuit-court number. The shape is \
             year + type + sequence, e.g. \"2024TR000321\" (traffic), \"2023SC000045\" (small \
             claims), \"2022CF000198\" (felony)."
        )));
    }
    let mut body = serde_json::json!({ "caseNo": case_no });
    match county_no {
        Some(n) => body["countyNo"] = serde_json::json!(n),
        None => warnings.push(
            "no --county given: this matches the case number in every Wisconsin county, so \
             several unrelated cases can come back. Pass a county to disambiguate."
                .to_string(),
        ),
    }
    let cases = post_search(WCCA_CASE_SEARCH, &body, warnings).await;
    if cases.is_empty() {
        warnings.push(format!(
            "no case {case_no} found. WCCA excludes sealed and expunged cases and some case \
             types by statute, so an empty result is not proof the case never existed."
        ));
    }
    Ok((cases, WCCA_CASE_SEARCH.to_string()))
}

/// County / case-type / filing-date-range search, walked in <=30-day windows.
async fn advanced_search(
    req: &StateCaseRequest,
    county_no: Option<u32>,
    limit: usize,
    warnings: &mut Vec<String>,
) -> Result<(Vec<WccaCase>, String)> {
    let case_type = req
        .case_type
        .as_deref()
        .map(|t| t.trim().to_ascii_uppercase())
        .filter(|t| !t.is_empty());

    // The upstream rejects a search with no date range ("Enter more
    // information for a valid search"), and a range alone across all 72
    // counties and all case types is a scrape, not a question.
    if county_no.is_none() && case_type.is_none() {
        return Err(Error::InvalidInput(
            "wisconsin case search needs either --case-no for one case, or a filing-date range \
             plus at least one of --county / --case-type. WCCA has no free-text party search on \
             the routes this tool uses."
                .to_string(),
        ));
    }
    let (start, end) = resolve_range(
        req.filed_after.as_deref(),
        req.filed_before.as_deref(),
        warnings,
    )?;

    let (windows, capped) = split_windows(start, end);
    if capped {
        warnings.push(format!(
            "range {start}..{end} exceeds the {MAX_WINDOWS} windows this tool will walk \
             ({} days at a time, WCCA's own maximum); only {start}..{} was searched. Narrow the \
             range to see the rest.",
            MAX_RANGE_DAYS + 1,
            windows.last().map(|w| w.1).unwrap_or(end)
        ));
    }

    let mut all: Vec<WccaCase> = Vec::new();
    for (i, (ws, we)) in windows.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(POLITE_DELAY_MS)).await;
        }
        let mut body = serde_json::json!({
            "filingDate": { "start": to_wcca_date(*ws), "end": to_wcca_date(*we) }
        });
        if let Some(n) = county_no {
            body["countyNo"] = serde_json::json!(n);
        }
        if let Some(t) = &case_type {
            body["caseType"] = serde_json::json!(t);
        }
        all.extend(post_search(WCCA_ADVANCED_SEARCH, &body, warnings).await);
        // Stop early rather than keep hitting a court's database for rows we
        // are about to throw away.
        if all.len() >= limit {
            if i + 1 < windows.len() {
                warnings.push(format!(
                    "stopped after {} of {} date windows: the limit of {limit} was already \
                     reached, so later dates in the range were not searched",
                    i + 1,
                    windows.len()
                ));
            }
            break;
        }
    }
    Ok((all, WCCA_ADVANCED_SEARCH.to_string()))
}

// ── upstream wire types ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct WccaEnvelope {
    #[serde(default)]
    result: Option<WccaResult>,
    /// Validation failures arrive here with a 200 status, not as an HTTP error
    /// — so `soft_fail` never sees them and they must be checked explicitly.
    #[serde(default)]
    errors: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct WccaResult {
    #[serde(default)]
    cases: Vec<WccaCase>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WccaCase {
    case_no: Option<String>,
    caption: Option<String>,
    party_name: Option<String>,
    county_name: Option<String>,
    county_no: Option<u32>,
    filing_date: Option<String>,
    status: Option<String>,
    dob: Option<String>,
    #[serde(default)]
    is_dob_sealed: bool,
}

/// The one place a date of birth can reach the response, so the redaction rule
/// lives here and nowhere else.
///
/// Two independent gates, both of which must open: the caller asked for it,
/// *and* WCCA has not flagged the record sealed. A sealed DOB is withheld even
/// from a caller who opted in — the flag is the court's decision, not ours.
fn to_record(c: WccaCase, include_dob: bool) -> StateCaseRecord {
    let dob = if include_dob && !c.is_dob_sealed {
        c.dob.clone()
    } else {
        None
    };
    let url = c.case_no.as_ref().map(|no| match c.county_no {
        Some(n) => format!("{WCCA_DETAIL_BASE}?countyNo={n}&caseNo={}", urlencoding::encode(no)),
        None => format!("{WCCA_DETAIL_BASE}?caseNo={}", urlencoding::encode(no)),
    });
    StateCaseRecord {
        case_no: c.case_no,
        caption: c.caption,
        party_name: c.party_name,
        county: c.county_name,
        filing_date: c.filing_date,
        status: c.status,
        dob,
        url,
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

async fn get_text(url: &str, source: &str, warnings: &mut Vec<String>) -> Option<String> {
    let resp = match shared_client::GENERAL.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("{source} request failed: {e}"));
            return None;
        }
    };
    let resp = soft_fail(source, resp, warnings).await?;
    match resp.text().await {
        Ok(t) => Some(t),
        Err(e) => {
            warnings.push(format!("{source} body read failed: {e}"));
            None
        }
    }
}

/// POST one WCCA search and return its rows. Every failure mode degrades into
/// `warnings` and an empty vec: a window that 500s should not lose the windows
/// that worked.
async fn post_search(
    url: &str,
    body: &serde_json::Value,
    warnings: &mut Vec<String>,
) -> Vec<WccaCase> {
    // One retry on a *transport* failure only. Measured: WCCA does not
    // throttle — 12 POSTs back-to-back with no delay and 8 consecutive 566 KB
    // responses all succeeded — but a connection can still be reset mid-body
    // on a long transfer, and losing a whole date window to one dropped socket
    // is a worse answer than pausing half a second. HTTP status codes are NOT
    // retried: a 403 or 429 means stop, not try harder.
    let mut attempt = 0;
    let resp = loop {
        match shared_client::GENERAL.post(url).json(body).send().await {
            Ok(r) => break r,
            Err(e) if attempt == 0 => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(POLITE_DELAY_MS * 2)).await;
                warnings.push(format!("WCCA connection dropped ({e}); retrying once"));
            }
            Err(e) => {
                warnings.push(format!("WCCA request failed: {e}"));
                return Vec::new();
            }
        }
    };
    let Some(resp) = soft_fail("WCCA", resp, warnings).await else {
        return Vec::new();
    };
    let raw = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            warnings.push(format!("WCCA body read failed: {e}"));
            return Vec::new();
        }
    };
    let env: WccaEnvelope = match serde_json::from_str(&raw) {
        Ok(e) => e,
        Err(e) => {
            warnings.push(format!(
                "WCCA returned something that is not the expected JSON ({e}); the route may have \
                 changed shape"
            ));
            return Vec::new();
        }
    };
    if let Some(errs) = &env.errors {
        let mut msgs = Vec::new();
        collect_error_strings(errs, &mut msgs);
        for m in msgs {
            warnings.push(format!("WCCA rejected the search: {m}"));
        }
    }
    env.result.unwrap_or_default().cases
}

/// WCCA nests validation messages under arbitrary field names
/// (`errors.filingDate.compositeError[]`, `errors._error[]`), so pull every
/// leaf string rather than pattern-match a shape that will drift.
fn collect_error_strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| collect_error_strings(x, out)),
        serde_json::Value::Object(o) => o.values().for_each(|x| collect_error_strings(x, out)),
        _ => {}
    }
}

/// Wisconsin encodes the chapter in the section number ("346.57" is ch. 346,
/// § 57), so `--chapter` is redundant here — but the shared request type also
/// serves Massachusetts, where it is required. Accept both spellings and
/// reject anything that is not a statute citation before spending a request.
fn normalize_section(section: &str, chapter: Option<&str>) -> Result<String> {
    let raw = section.trim().trim_start_matches('§').trim();
    let raw = raw.strip_prefix("s.").unwrap_or(raw).trim();
    if raw.is_empty() {
        return Err(Error::InvalidInput(
            "wisconsin statute needs a section, e.g. \"346.57\"".to_string(),
        ));
    }
    // "chapter 346, section 57" -> "346.57", but only when the section is not
    // already fully qualified.
    let joined = match chapter {
        Some(ch) if !raw.contains('.') => format!("{}.{raw}", ch.trim()),
        _ => raw.to_string(),
    };
    if !SECTION_RE.is_match(&joined) {
        return Err(Error::InvalidInput(format!(
            "{joined:?} is not a Wisconsin statute section. Expected chapter.section, optionally \
             with subsections: \"346.57\", \"346.57(4)(a)\", \"940.01\"."
        )));
    }
    Ok(joined)
}

/// Compiled once: these run per request and the patterns are static.
static SECTION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    // chapter (digits, optional trailing letter for e.g. 100A) "." section
    // (digits, optional trailing letters) plus any number of (sub) parts.
    regex::Regex::new(r"^\d{1,3}[A-Za-z]?\.\d{1,4}[A-Za-z]*(\([0-9A-Za-z]+\))*$")
        .expect("static section regex")
});

/// Year + case-type + sequence, as printed on a Wisconsin citation.
static CASE_NO_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^(19|20)\d{2}[A-Z]{2,3}\d{1,7}$").expect("static case regex"));

/// `<a class="reference">346.57(1)</a>` repeats the number that the very next
/// span renders anyway; left in, every paragraph reads "346.57(1)(1) ...".
static REFERENCE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r#"(?is)<(?:a|span)\b[^>]*class="[^"]*\breference\b[^"]*"[^>]*>[^<]*</(?:a|span)>"#)
        .expect("static reference regex")
});

fn looks_like_case_no(s: &str) -> bool {
    CASE_NO_RE.is_match(s.trim())
}

/// "346.57(4)(a)" -> "346".
fn chapter_of(section: &str) -> Option<String> {
    let head = section.split('.').next()?.trim();
    (!head.is_empty()).then(|| head.to_string())
}

/// Pull the blocks belonging to one section out of the page.
///
/// The statutes site serves a whole *subchapter* per page — asking for 346.57
/// lands on a page that also carries 346.53 through 346.60 — so the section
/// cannot be read off the page as a whole. Every text block carries
/// `data-section="346.57"`, which is the only reliable boundary, and the
/// blocks are siblings rather than nested. Depth is still counted rather than
/// assumed, so a future nested `<div>` degrades to "too much text" instead of
/// "text cut in half".
fn section_blocks(html: &str, section: &str) -> Vec<String> {
    let needle = format!("data-section=\"{section}\"");
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(hit) = html[cursor..].find(&needle) {
        let attr_at = cursor + hit;
        // Back up to the '<' that opens this element, then forward to its
        // '>'. If a '>' sits between the two, the needle was in text content
        // rather than an attribute — skip it instead of slicing at a tag that
        // is not there.
        let inside_tag = html[..attr_at]
            .rfind('<')
            .is_some_and(|open| !html[open..attr_at].contains('>'));
        if !inside_tag {
            cursor = attr_at + needle.len();
            continue;
        }
        let Some(gt) = html[attr_at..].find('>') else { break };
        let body_start = attr_at + gt + 1;
        let end = match_close_div(html, body_start);
        let raw = &html[body_start..end];
        let cleaned = REFERENCE_RE.replace_all(raw, " ");
        let text = strip_markup(&cleaned);
        if !text.is_empty() {
            out.push(text);
        }
        cursor = end;
    }
    out
}

/// Byte offset of the `</div>` that closes the element whose body starts at
/// `from`, counting nested opens.
fn match_close_div(html: &str, from: usize) -> usize {
    let rest = &html[from..];
    let mut depth = 1usize;
    let mut i = 0usize;
    while i < rest.len() {
        let Some(next) = rest[i..].find('<') else { break };
        let at = i + next;
        if rest[at..].starts_with("</div") {
            depth -= 1;
            if depth == 0 {
                return from + at;
            }
            i = at + 5;
        } else if rest[at..].starts_with("<div") {
            depth += 1;
            i = at + 4;
        } else {
            i = at + 1;
        }
    }
    html.len()
}

/// "346.57 Speed restrictions." -> "Speed restrictions."
fn strip_leading_section(head: &str, section: &str) -> String {
    head.trim()
        .strip_prefix(section)
        .unwrap_or(head)
        .trim()
        .to_string()
}

/// Conservative repeal detection.
///
/// Wisconsin removes repealed sections from the code outright rather than
/// leaving "[Repealed]" stubs, so the signal is a note *inside* the section
/// saying so. The trap: notes routinely mention that some *other* section was
/// repealed ("NOTE: Section 23.33 (11m) was repealed by 2009 Wis. Act 175"
/// sits inside a perfectly current § 346.94). Matching a bare "repealed" would
/// flag half the traffic code, so only self-referential phrasing counts.
fn looks_repealed(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("this section was repealed")
        || lower.contains("this section is repealed")
        || lower.contains("[repealed]")
}

/// Accepts "Milwaukee", "milwaukee county", "St. Croix", "Saint Croix", or a
/// bare number. Returns `None` for anything unrecognised — the caller turns
/// that into an error rather than searching some other county.
fn county_number(input: &str) -> Option<u32> {
    let raw = input.trim();
    if let Ok(n) = raw.parse::<u32>() {
        return COUNTIES.iter().find(|(no, _)| *no == n).map(|(no, _)| *no);
    }
    let key = county_key(raw);
    if key.is_empty() {
        return None;
    }
    COUNTIES
        .iter()
        .find(|(_, name)| county_key(name) == key)
        .map(|(no, _)| *no)
}

/// Fold the spellings a caller will actually type onto one key: case,
/// a trailing "county", the period in "St. Croix", and "Saint" for "St".
fn county_key(input: &str) -> String {
    let lower = input.trim().to_ascii_lowercase();
    let lower = lower
        .strip_suffix(" county")
        .map(str::to_string)
        .unwrap_or(lower);
    let mut words: Vec<String> = Vec::new();
    for w in lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        if w.is_empty() {
            continue;
        }
        words.push(if w == "saint" { "st".to_string() } else { w.to_string() });
    }
    words.join(" ")
}

fn county_names() -> Vec<&'static str> {
    COUNTIES.iter().map(|(_, n)| *n).collect()
}

/// WCCA's own form posts MM-DD-YYYY. (It also accepts ISO, but the form format
/// is the one their validator echoes back, so it is the one we send.)
fn to_wcca_date(d: NaiveDate) -> String {
    d.format("%m-%d-%Y").to_string()
}

/// Turn the contract's optional ISO bounds into a concrete range.
///
/// One-sided ranges are common ("everything since March 1") and the upstream
/// rejects them outright, so fill the missing side with the widest window it
/// will accept rather than erroring.
fn resolve_range(
    after: Option<&str>,
    before: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<(NaiveDate, NaiveDate)> {
    let start = after.map(|d| parse_date(d, "--filed-after")).transpose()?;
    let end = before.map(|d| parse_date(d, "--filed-before")).transpose()?;
    let today = Utc::now().date_naive();
    let (start, end) = match (start, end) {
        (Some(s), Some(e)) => (s, e),
        (Some(s), None) => {
            let e = (s + Duration::days(MAX_RANGE_DAYS)).min(today);
            warnings.push(format!(
                "no --filed-before given; searched {s}..{e} (WCCA needs a bounded filing-date \
                 range)"
            ));
            (s, e)
        }
        (None, Some(e)) => {
            let s = e - Duration::days(MAX_RANGE_DAYS);
            warnings.push(format!("no --filed-after given; searched {s}..{e}"));
            (s, e)
        }
        (None, None) => {
            return Err(Error::InvalidInput(
                "wisconsin case search needs a filing-date range (--filed-after / \
                 --filed-before), or --case-no for a single case. WCCA rejects an unbounded \
                 search."
                    .to_string(),
            ));
        }
    };
    if end < start {
        return Err(Error::InvalidInput(format!(
            "--filed-before {end} is earlier than --filed-after {start}"
        )));
    }
    Ok((start, end))
}

/// Chop a range into windows WCCA will accept, capped. Returns the windows and
/// whether the cap cut the range short.
fn split_windows(start: NaiveDate, end: NaiveDate) -> (Vec<(NaiveDate, NaiveDate)>, bool) {
    let mut windows = Vec::new();
    let mut cur = start;
    while cur <= end {
        if windows.len() == MAX_WINDOWS {
            return (windows, true);
        }
        let stop = (cur + Duration::days(MAX_RANGE_DAYS)).min(end);
        windows.push((cur, stop));
        cur = stop + Duration::days(1);
    }
    (windows, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── dates ──────────────────────────────────────────────────────────────

    #[test]
    fn iso_dates_become_wccas_mm_dd_yyyy() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 2).expect("valid date");
        assert_eq!(to_wcca_date(d), "01-02-2024");
        let d = NaiveDate::from_ymd_opt(2023, 12, 31).expect("valid date");
        assert_eq!(to_wcca_date(d), "12-31-2023");
    }

    #[test]
    fn one_sided_range_is_filled_not_rejected() {
        let mut w = Vec::new();
        let (s, e) = resolve_range(Some("2024-01-02"), None, &mut w).expect("fills the open end");
        assert_eq!(s.to_string(), "2024-01-02");
        assert_eq!(e.to_string(), "2024-01-31", "one WCCA-legal window");
        assert!(w.iter().any(|m| m.contains("no --filed-before")), "{w:?}");

        let mut w = Vec::new();
        assert!(resolve_range(None, None, &mut w).is_err(), "unbounded must be refused");
        let mut w = Vec::new();
        assert!(
            resolve_range(Some("2024-03-01"), Some("2024-02-01"), &mut w).is_err(),
            "reversed range must be refused"
        );
    }

    #[test]
    fn ranges_are_split_into_windows_wcca_accepts() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("valid date");
        // Upstream rejects anything wider than 30 days, so no window may be.
        let (w, capped) = split_windows(d("2024-01-01"), d("2024-04-30"));
        assert!(!capped);
        assert_eq!(w.len(), 5, "121 days at 30 per window");
        for (s, e) in &w {
            assert!((*e - *s).num_days() <= MAX_RANGE_DAYS, "{s}..{e} is too wide for WCCA");
        }
        // Windows must tile without gaps or overlap, or cases go missing.
        for pair in w.windows(2) {
            assert_eq!(pair[1].0 - pair[0].1, Duration::days(1));
        }
        assert_eq!(w[0], (d("2024-01-01"), d("2024-01-30")));
        assert_eq!(
            w.last().map(|x| x.1),
            Some(d("2024-04-30")),
            "the last window ends on the requested date, never past it"
        );

        let (w, capped) = split_windows(d("2020-01-01"), d("2024-12-31"));
        assert!(capped, "a five-year range must be reported as cut short");
        assert_eq!(w.len(), MAX_WINDOWS);
    }

    // ── counties ───────────────────────────────────────────────────────────

    #[test]
    fn county_names_resolve_to_the_numbers_wcca_returned() {
        assert_eq!(county_number("Milwaukee"), Some(40));
        assert_eq!(county_number("milwaukee"), Some(40));
        assert_eq!(county_number("  Milwaukee County "), Some(40));
        assert_eq!(county_number("40"), Some(40));
        // Menominee is 72, not alphabetical — the row that proves the table
        // was read back from the API rather than generated from a sort.
        assert_eq!(county_number("Menominee"), Some(72));
        assert_eq!(county_number("Dane"), Some(13));
        // Punctuation and the "Saint" spelling fold onto the same key.
        assert_eq!(county_number("St. Croix"), Some(55));
        assert_eq!(county_number("st croix"), Some(55));
        assert_eq!(county_number("Saint Croix"), Some(55));
        assert_eq!(county_number("Fond du Lac"), Some(20));
        assert_eq!(county_number("EAU CLAIRE"), Some(18));
    }

    #[test]
    fn unknown_county_is_refused_rather_than_guessed() {
        // A wrong number does not error upstream — it returns another county's
        // docket — so anything unrecognised must stop here.
        assert_eq!(county_number("Cook"), None);
        assert_eq!(county_number("Milwuakee"), None);
        assert_eq!(county_number("0"), None);
        assert_eq!(county_number("73"), None);
        assert_eq!(county_number(""), None);
    }

    #[test]
    fn county_table_is_complete_and_unique() {
        assert_eq!(COUNTIES.len(), 72, "Wisconsin has 72 counties");
        let mut nums: Vec<u32> = COUNTIES.iter().map(|(n, _)| *n).collect();
        nums.sort_unstable();
        nums.dedup();
        assert_eq!(nums.len(), 72, "duplicate county number");
        assert_eq!(nums.first(), Some(&1));
        assert_eq!(nums.last(), Some(&72));
        let mut keys: Vec<String> = COUNTIES.iter().map(|(_, n)| county_key(n)).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), 72, "two counties fold onto the same lookup key");
    }

    // ── case numbers ───────────────────────────────────────────────────────

    #[test]
    fn case_number_shape_is_validated_before_spending_a_request() {
        assert!(looks_like_case_no("2024TR000321"));
        assert!(looks_like_case_no("2023SC000045"));
        assert!(looks_like_case_no("1998CF000198"));
        assert!(!looks_like_case_no("TR000321"), "missing year");
        assert!(!looks_like_case_no("2024000321"), "missing case type");
        assert!(!looks_like_case_no("24TR000321"), "two-digit year");
        assert!(!looks_like_case_no("2024tr000321"), "lowercase is uppercased by the caller");
        assert!(!looks_like_case_no("2024TR000321; DROP"), "trailing junk");
        assert!(!looks_like_case_no(""));
    }

    // ── PII ────────────────────────────────────────────────────────────────

    fn sample(sealed: bool) -> WccaCase {
        WccaCase {
            case_no: Some("2024TR000321".to_string()),
            caption: Some("State of Wisconsin vs. A Person".to_string()),
            party_name: Some("Person, A".to_string()),
            county_name: Some("Milwaukee".to_string()),
            county_no: Some(40),
            filing_date: Some("2024-01-02".to_string()),
            status: Some("Closed".to_string()),
            dob: Some("1990-07".to_string()),
            is_dob_sealed: sealed,
        }
    }

    #[test]
    fn dob_is_dropped_unless_the_caller_opted_in() {
        let r = to_record(sample(false), false);
        assert_eq!(r.dob, None, "include_dob=false must not carry a DOB through");
        // Everything else still comes back — the caption is the record.
        assert_eq!(r.party_name.as_deref(), Some("Person, A"));
        assert_eq!(r.case_no.as_deref(), Some("2024TR000321"));
    }

    #[test]
    fn sealed_dob_is_dropped_even_when_the_caller_opted_in() {
        let r = to_record(sample(true), true);
        assert_eq!(r.dob, None, "isDobSealed is the court's call, not the caller's");
        // ...and the opt-in does work when the record is not sealed, or the
        // test above would pass for the wrong reason.
        let r = to_record(sample(false), true);
        assert_eq!(r.dob.as_deref(), Some("1990-07"));
    }

    #[test]
    fn redacted_dob_is_omitted_from_json_entirely() {
        // skip_serializing_if on the field means "absent", not "null" — a null
        // still tells a reader the field exists and was withheld for this row.
        let json = serde_json::to_string(&to_record(sample(false), false)).expect("serializes");
        assert!(!json.contains("dob"), "{json}");
        let json = serde_json::to_string(&to_record(sample(false), true)).expect("serializes");
        assert!(json.contains("1990-07"), "{json}");
    }

    #[test]
    fn detail_url_points_at_the_human_page() {
        let r = to_record(sample(false), false);
        assert_eq!(
            r.url.as_deref(),
            Some("https://wcca.wicourts.gov/caseDetail.html?countyNo=40&caseNo=2024TR000321")
        );
    }

    // ── statute parsing ────────────────────────────────────────────────────

    /// Trimmed from the live page for 346.57, which serves the whole of
    /// subchapter IX: two sections' blocks interleaved, exactly as upstream.
    const SUBCHAPTER_FIXTURE: &str = r#"
<div class="qsatxt_1sect level3" data-path="/statutes/statutes/346/ix/57" data-section="346.57" data-cites='["statutes/346.57"]'><a class="reference" href="/document/statutes/346.57">346.57</a><span class="qsnum_sect"><span class="qstr">346.57</span></span><span class="qstr"> </span><span class="qstitle_sect"><span class="qstr">Speed restrictions.</span></span></div>
<div class="qsatxt_2subsect level4" data-path="/statutes/statutes/346/ix/57/2" data-section="346.57" data-cites='["statutes/346.57(2)"]'><a class="reference" href="/document/statutes/346.57(2)">346.57(2)</a><span class="qsnum_subsect"><span class="qstr">(2)</span></span><span class="qstr"> </span><span class="qstitle_subsect"><span class="qstr">Reasonable and prudent limit.</span></span><span class="qstr">  No person shall drive a vehicle at a speed greater than is reasonable and prudent.</span></div>
<div class="qsatxt_1sect level3" data-path="/statutes/statutes/346/ix/58" data-section="346.58" data-cites='["statutes/346.58"]'><a class="reference" href="/document/statutes/346.58">346.58</a><span class="qstr">346.58 Special speed limits.</span></div>
"#;

    /// The ~12 KB body this host returns for a section that does not exist.
    /// It is a 404, but callers that only check `status.is_success()` on a
    /// redirect chain would still be holding a page, so the parser must reject
    /// it on content.
    const NOT_FOUND_FIXTURE: &str = r#"<!DOCTYPE html><html><head><title>Wisconsin Legislature: 940.205</title></head>
<body><div class="alert">Stats. 940.205 not found</div>
<div class="nav"><a href="/statutes">Statutes</a></div></body></html>"#;

    #[test]
    fn extracts_only_the_requested_sections_blocks() {
        let blocks = section_blocks(SUBCHAPTER_FIXTURE, "346.57");
        assert_eq!(blocks.len(), 2, "the neighbouring 346.58 block must not be picked up: {blocks:?}");
        assert!(blocks[0].starts_with("346.57 Speed restrictions"), "{:?}", blocks[0]);
        // The <a class="reference"> duplicate of the number is dropped, so the
        // paragraph does not read "346.57(2)(2) Reasonable...".
        assert!(
            blocks[1].starts_with("(2) Reasonable and prudent limit."),
            "{:?}",
            blocks[1]
        );
        assert!(blocks[1].contains("reasonable and prudent."));

        assert_eq!(section_blocks(SUBCHAPTER_FIXTURE, "346.58").len(), 1);
        // A section that merely appears in a data-cites list is not present.
        assert!(section_blocks(SUBCHAPTER_FIXTURE, "346.59").is_empty());
    }

    #[test]
    fn a_404_page_is_rejected_on_content_not_status() {
        assert!(
            section_blocks(NOT_FOUND_FIXTURE, "940.205").is_empty(),
            "the not-found chrome page carries no section text and must not be returned as law"
        );
        assert!(NOT_FOUND_FIXTURE.len() > 200, "fixture is the shape of the real body");
    }

    #[test]
    fn heading_drops_the_number_already_in_the_citation() {
        let blocks = section_blocks(SUBCHAPTER_FIXTURE, "346.57");
        assert_eq!(
            strip_leading_section(&blocks[0], "346.57"),
            "Speed restrictions."
        );
    }

    #[test]
    fn repeal_detection_ignores_notes_about_other_sections() {
        // Verbatim shape of the note inside the current § 346.94.
        assert!(
            !looks_repealed("NOTE: Section 23.33 (11m) was repealed by 2009 Wis. Act 175."),
            "a note about another section must not flag this one as repealed"
        );
        // Verbatim shape of the note inside § 757.57.
        assert!(looks_repealed(
            "NOTE:  This section was repealed by Sup. Ct. Order dated 12-11-79, eff. 1-1-80."
        ));
        assert!(!looks_repealed("Speed restrictions. No person shall drive..."));
    }

    #[test]
    fn nested_divs_do_not_truncate_a_block() {
        let html = r#"<div data-section="1.01"><span>a</span><div>b</div>c</div><div data-section="1.02">z</div>"#;
        let blocks = section_blocks(html, "1.01");
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].contains('c'), "closing tag matching stopped early: {blocks:?}");
    }

    // ── section normalization ──────────────────────────────────────────────

    #[test]
    fn section_accepts_the_forms_a_citation_is_written_in() {
        assert_eq!(normalize_section("346.57", None).expect("plain"), "346.57");
        assert_eq!(normalize_section(" § 346.57 ", None).expect("with sign"), "346.57");
        assert_eq!(normalize_section("s. 346.57", None).expect("wi style"), "346.57");
        assert_eq!(
            normalize_section("346.57(4)(a)", None).expect("subsections"),
            "346.57(4)(a)"
        );
        // Chapter is redundant for WI but arrives from the shared request type.
        assert_eq!(normalize_section("57", Some("346")).expect("joined"), "346.57");
        assert_eq!(
            normalize_section("346.57", Some("346")).expect("already qualified"),
            "346.57"
        );
    }

    #[test]
    fn section_rejects_things_that_are_not_citations() {
        assert!(normalize_section("", None).is_err());
        assert!(normalize_section("speeding", None).is_err());
        assert!(normalize_section("346", None).is_err(), "a bare chapter is not a section");
        assert!(normalize_section("../../etc/passwd", None).is_err());
    }

    #[test]
    fn chapter_is_derived_for_the_fallback_link() {
        assert_eq!(chapter_of("346.57").as_deref(), Some("346"));
        assert_eq!(chapter_of("940.01(1)(a)").as_deref(), Some("940"));
    }

    // ── upstream envelope ──────────────────────────────────────────────────

    #[test]
    fn wcca_validation_errors_arrive_with_a_200_and_are_flattened() {
        // Verbatim body from a 60-day range, which the upstream refuses.
        let raw = r#"{"errors":{"filingDate":{"compositeError":["Invalid date range. Date range cannot be greater than 30 days."]}},"view":{}}"#;
        let env: WccaEnvelope = serde_json::from_str(raw).expect("parses");
        assert!(env.result.is_none());
        let mut msgs = Vec::new();
        collect_error_strings(&env.errors.expect("errors present"), &mut msgs);
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].contains("greater than 30 days"), "{msgs:?}");
    }

    #[test]
    fn wcca_case_rows_deserialize_from_the_live_shape() {
        let raw = r#"{"result":{"cases":[{"partyName":"Doe, Jane","countyName":"Milwaukee","dob":"1990-07","caseNo":"2024TR000321","countyNo":40,"caption":"State of Wisconsin vs. Jane Doe","status":"Closed","filingDate":"2024-01-02","isDobSealed":false}]},"view":{}}"#;
        let env: WccaEnvelope = serde_json::from_str(raw).expect("parses");
        let cases = env.result.expect("result present").cases;
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].county_no, Some(40));
        assert!(!cases[0].is_dob_sealed);
    }

    // ── live smoke tests ───────────────────────────────────────────────────
    //
    // Ignored by default: everything above is pure logic against captured
    // fixtures, and `cargo test` must not depend on a state court's uptime.
    // Run with `cargo test -p eli-core legal::states::wisconsin -- --ignored`
    // when changing a parser, to confirm the live shape still matches.

    #[tokio::test]
    #[ignore = "hits docs.legis.wisconsin.gov"]
    async fn live_statute_346_57() {
        let r = fetch_statute(StateStatuteRequest {
            state: "wi".to_string(),
            section: "346.57".to_string(),
            chapter: None,
            max_chars: 200_000,
        })
        .await
        .expect("statute fetch");
        assert_eq!(r.citation.as_deref(), Some("Wis. Stat. § 346.57"));
        assert_eq!(r.heading.as_deref(), Some("Speed restrictions."));
        let text = r.text.expect("text");
        assert!(text.contains("reasonable and prudent"), "{}", &text[..400.min(text.len())]);
        // The neighbouring sections on the same subchapter page must not leak in.
        assert!(!text.contains("346.58"), "picked up an adjacent section");
        assert_eq!(r.repealed, Some(false));
    }

    #[tokio::test]
    #[ignore = "hits docs.legis.wisconsin.gov"]
    async fn live_statute_missing_section_degrades() {
        let r = fetch_statute(StateStatuteRequest {
            state: "wi".to_string(),
            section: "940.205".to_string(),
            chapter: None,
            max_chars: 10_000,
        })
        .await
        .expect("must degrade, not error");
        assert!(r.text.is_none());
        assert!(r.citation.is_none(), "an absent section must not be cited");
        assert!(r.warnings.iter().any(|w| w.contains("not in the current statutes")), "{:?}", r.warnings);
    }

    #[tokio::test]
    #[ignore = "hits wcca.wicourts.gov"]
    async fn live_single_case() {
        let r = fetch_cases(StateCaseRequest {
            state: "wi".to_string(),
            county: Some("Milwaukee".to_string()),
            case_type: None,
            case_no: Some("2024TR000001".to_string()),
            filed_after: None,
            filed_before: None,
            limit: 10,
            include_dob: false,
        })
        .await
        .expect("case fetch");
        assert_eq!(r.returned, 1);
        let c = &r.cases[0];
        assert_eq!(c.case_no.as_deref(), Some("2024TR000001"));
        assert_eq!(c.county.as_deref(), Some("Milwaukee"));
        assert!(c.party_name.is_some(), "the caption is always populated");
        assert_eq!(c.dob, None, "include_dob=false");
    }

    #[tokio::test]
    #[ignore = "hits wcca.wicourts.gov"]
    async fn live_advanced_search_one_day_of_milwaukee_traffic() {
        let r = fetch_cases(StateCaseRequest {
            state: "wi".to_string(),
            county: Some("milwaukee".to_string()),
            case_type: Some("TR".to_string()),
            case_no: None,
            filed_after: Some("2024-01-02".to_string()),
            filed_before: Some("2024-01-02".to_string()),
            limit: 500,
            include_dob: false,
        })
        .await
        .expect("advanced search");
        assert!(r.returned > 200, "expected a few hundred, got {}", r.returned);
        assert!(r.cases.iter().all(|c| c.dob.is_none()));
        assert!(r.cases.iter().all(|c| c.county.as_deref() == Some("Milwaukee")));
    }

    #[test]
    fn missing_fields_do_not_break_the_row() {
        // WCCA omits `dob` entirely on some rows rather than sending null.
        let raw = r#"{"result":{"cases":[{"caseNo":"2024SC000001"}]}}"#;
        let env: WccaEnvelope = serde_json::from_str(raw).expect("parses");
        let cases = env.result.expect("result present").cases;
        assert_eq!(cases[0].dob, None);
        assert!(!cases[0].is_dob_sealed, "an absent seal flag defaults to not sealed");
        // ...and an absent flag must still not leak a DOB that isn't there.
        assert_eq!(to_record(cases[0].clone(), true).dob, None);
    }
}
