//! eCFR — the Code of Federal Regulations, including its history.
//!
//! Why this exists when the CFR is already on the open web: ecfr.gov serves the
//! text of any citation *as it read on any date back to 2017*, plus the full
//! amendment log for a part. A search engine indexes the current text only —
//! ask it what 17 CFR 240.10b5-1 said in June 2021 and you get the 2022
//! rewrite, silently. Compliance and securities work is mostly about the
//! version in force at the time of the conduct, so "current" is the wrong
//! answer more often than not.
//!
//! No API key. Base: https://www.ecfr.gov/api/

use crate::legal::{clamp_text, parse_date, shared_client, soft_fail, strip_markup};
use crate::{Error, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

const ECFR_BASE: &str = "https://www.ecfr.gov/api";
/// Measured floor: 2016-12-31 answers, 2016-12-30 404s — and it 404s with the
/// same message as a bad section number, so we range-check here rather than
/// letting the caller misread "no such date" as "no such regulation".
const ECFR_EARLIEST: &str = "2016-12-31";

#[derive(Clone, Debug)]
pub struct CfrRequest {
    pub title: Option<u32>,
    pub part: Option<String>,
    pub section: Option<String>,
    pub subpart: Option<String>,
    /// Point-in-time date (YYYY-MM-DD). None = current text.
    pub date: Option<String>,
    /// Full-text search instead of a citation fetch.
    pub query: Option<String>,
    pub history: bool,
    pub structure: bool,
    /// Second date — when set, diff the text between `date` and `diff_date`.
    pub diff_date: Option<String>,
    pub max_chars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CfrAmendment {
    /// Date the amendment took effect in the CFR.
    pub date: String,
    pub issue_date: Option<String>,
    /// e.g. "240.10b5-1" or "Appendix A to Part 1026".
    pub identifier: String,
    pub name: Option<String>,
    pub part: Option<String>,
    pub subpart: Option<String>,
    /// eCFR's own flag: false means an editorial/technical touch, not a real change.
    pub substantive: bool,
    pub removed: bool,
    pub node_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CfrSearchHit {
    pub citation: String,
    pub title: Option<u32>,
    pub part: Option<String>,
    pub section: Option<String>,
    pub heading: Option<String>,
    pub snippet: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CfrDiff {
    pub from_date: String,
    pub to_date: String,
    pub changed: bool,
    pub chars_from: usize,
    pub chars_to: usize,
    /// Human-readable unified-ish diff of the two texts, clamped.
    pub diff: String,
    pub lines_added: usize,
    pub lines_removed: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CfrResponse {
    pub generated_at: DateTime<Utc>,
    /// "text" | "history" | "structure" | "search" | "diff"
    pub mode: String,
    pub citation: Option<String>,
    /// The date the returned text is actually in force on.
    pub as_of: Option<String>,
    pub title: Option<u32>,
    pub part: Option<String>,
    pub section: Option<String>,
    pub heading: Option<String>,
    pub text: Option<String>,
    pub chars: usize,
    pub truncated: bool,
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub amendments: Vec<CfrAmendment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structure: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<CfrSearchHit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<CfrDiff>,
    pub warnings: Vec<String>,
}

impl CfrResponse {
    fn empty(mode: &str) -> Self {
        Self {
            generated_at: Utc::now(),
            mode: mode.to_string(),
            citation: None,
            as_of: None,
            title: None,
            part: None,
            section: None,
            heading: None,
            text: None,
            chars: 0,
            truncated: false,
            source_url: None,
            amendments: Vec::new(),
            structure: None,
            results: Vec::new(),
            diff: None,
            warnings: Vec::new(),
        }
    }
}

pub async fn fetch_cfr(req: CfrRequest) -> Result<CfrResponse> {
    if let Some(q) = req.query.clone() {
        return search_cfr(&q, &req).await;
    }
    let title = req
        .title
        .ok_or_else(|| Error::InvalidInput("cfr requires --title, or --q to search".into()))?;

    if req.history {
        return history(title, &req).await;
    }
    if req.structure {
        return structure(title, &req).await;
    }
    if req.diff_date.is_some() {
        return diff(title, &req).await;
    }
    text(title, &req).await
}

/// Resolve the requested point-in-time date to one eCFR will actually serve.
///
/// Two traps here. Dates before the corpus floor 404 with the same message as a
/// bad section number. And "today" is usually NOT a valid issue date: a title's
/// latest issue lags the calendar by days to weeks, and asking past it 404s —
/// so the naive "no date means now" default breaks the most common call of all,
/// fetching current text. `latest_issue_date` per title is the real ceiling.
async fn resolve_date(
    title: u32,
    date: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<String> {
    let Some(d) = date else {
        return Ok(match latest_issue_date(title, warnings).await {
            Some(latest) => latest,
            None => Utc::now().date_naive().to_string(),
        });
    };
    let parsed = parse_date(d, "--date")?;
    let earliest = NaiveDate::parse_from_str(ECFR_EARLIEST, "%Y-%m-%d").expect("const date");
    if parsed < earliest {
        warnings.push(format!(
            "eCFR coverage starts {ECFR_EARLIEST}; {d} predates it. For older text use the annual \
             GPO annual CFR editions on govinfo, which go back to 1996."
        ));
    }
    let today = Utc::now().date_naive();
    if parsed > today {
        return Err(Error::InvalidInput(format!(
            "--date {d} is in the future; the CFR only exists up to {today}"
        )));
    }
    // Clamp forward-of-corpus dates rather than 404ing: a caller asking for
    // "today" on a title issued last week means "the current text".
    if let Some(latest) = latest_issue_date(title, warnings).await {
        if let Ok(latest_date) = NaiveDate::parse_from_str(&latest, "%Y-%m-%d") {
            if parsed > latest_date {
                warnings.push(format!(
                    "title {title} has no issue after {latest}; returning the text in force then                      rather than on {d}"
                ));
                return Ok(latest);
            }
        }
    }
    Ok(parsed.to_string())
}

/// The most recent issue date eCFR holds for a title.
async fn latest_issue_date(title: u32, warnings: &mut Vec<String>) -> Option<String> {
    let url = format!("{ECFR_BASE}/versioner/v1/titles.json");
    let body = get_text(&url, "ecfr titles", warnings).await?;
    let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
    parsed
        .get("titles")?
        .as_array()?
        .iter()
        .find(|t| t.get("number").and_then(|n| n.as_u64()) == Some(title as u64))
        .and_then(|t| {
            t.get("latest_issue_date")
                .or_else(|| t.get("up_to_date_as_of"))
        })
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

fn citation_of(title: u32, part: Option<&str>, section: Option<&str>) -> String {
    match (part, section) {
        (_, Some(s)) => format!("{title} CFR {s}"),
        (Some(p), None) => format!("{title} CFR {p}"),
        (None, None) => format!("{title} CFR"),
    }
}

/// Fetch regulation text at a point in time.
async fn text(title: u32, req: &CfrRequest) -> Result<CfrResponse> {
    let mut out = CfrResponse::empty("text");
    let date = resolve_date(title, req.date.as_deref(), &mut out.warnings).await?;

    // Section implies its part — eCFR wants both, and callers reliably pass
    // only the section ("240.10b5-1"), so derive the part from it.
    let part = req
        .part
        .clone()
        .or_else(|| req.section.as_ref().and_then(|s| part_of_section(s)));

    if part.is_none() && req.subpart.is_none() {
        out.warnings.push(
            "no --part given: fetching a whole CFR title is megabytes of XML. Narrow with --part \
             or --section."
                .to_string(),
        );
    }

    let mut url = format!("{ECFR_BASE}/versioner/v1/full/{date}/title-{title}.xml");
    let mut qs: Vec<String> = Vec::new();
    if let Some(p) = part.as_deref() {
        qs.push(format!("part={}", urlencoding::encode(p)));
    }
    if let Some(sp) = req.subpart.as_deref() {
        qs.push(format!("subpart={}", urlencoding::encode(sp)));
    }
    if let Some(s) = req.section.as_deref() {
        qs.push(format!("section={}", urlencoding::encode(s)));
    }
    if !qs.is_empty() {
        url = format!("{url}?{}", qs.join("&"));
    }

    let body = match get_text(&url, "ecfr full", &mut out.warnings).await {
        Some(b) => b,
        None => {
            out.source_url = Some(url);
            out.as_of = Some(date);
            out.title = Some(title);
            return Ok(out);
        }
    };

    let heading = first_head(&body);
    let plain = strip_markup(&body);
    let (clamped, truncated) = clamp_text(&plain, req.max_chars);

    out.citation = Some(citation_of(title, part.as_deref(), req.section.as_deref()));
    out.as_of = Some(date);
    out.title = Some(title);
    out.part = part;
    out.section = req.section.clone();
    out.heading = heading;
    out.chars = plain.chars().count();
    out.truncated = truncated;
    out.text = Some(clamped);
    out.source_url = Some(url);
    Ok(out)
}

/// Every amendment to a part, with eCFR's own substantive/editorial flag.
async fn history(title: u32, req: &CfrRequest) -> Result<CfrResponse> {
    let mut out = CfrResponse::empty("history");
    let part = req
        .part
        .clone()
        .or_else(|| req.section.as_ref().and_then(|s| part_of_section(s)));

    let mut url = format!("{ECFR_BASE}/versioner/v1/versions/title-{title}.json");
    let mut qs: Vec<String> = Vec::new();
    if let Some(p) = part.as_deref() {
        qs.push(format!("part={}", urlencoding::encode(p)));
    }
    if let Some(d) = req.date.as_deref() {
        let d = parse_date(d, "--date")?;
        qs.push(format!("issue_date%5Bgte%5D={d}"));
    }
    if !qs.is_empty() {
        url = format!("{url}?{}", qs.join("&"));
    }

    let Some(body) = get_text(&url, "ecfr versions", &mut out.warnings).await else {
        out.source_url = Some(url);
        return Ok(out);
    };

    let parsed: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Error::Provider(format!("ecfr versions parse failed: {e}")))?;

    if let Some(arr) = parsed.get("content_versions").and_then(|v| v.as_array()) {
        for v in arr {
            let identifier = v
                .get("identifier")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            if let Some(sec) = req.section.as_deref() {
                if !identifier.eq_ignore_ascii_case(sec) {
                    continue;
                }
            }
            out.amendments.push(CfrAmendment {
                date: v
                    .get("date")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
                issue_date: v
                    .get("issue_date")
                    .and_then(|x| x.as_str())
                    .map(str::to_string),
                identifier,
                name: v.get("name").and_then(|x| x.as_str()).map(str::to_string),
                part: v.get("part").and_then(|x| x.as_str()).map(str::to_string),
                subpart: v.get("subpart").and_then(|x| x.as_str()).map(str::to_string),
                substantive: v
                    .get("substantive")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(true),
                removed: v.get("removed").and_then(|x| x.as_bool()).unwrap_or(false),
                node_type: v.get("type").and_then(|x| x.as_str()).map(str::to_string),
            });
        }
    }

    // eCFR emits the same amendment more than once — typically an editorial
    // re-issue alongside the substantive row, differing only in issue_date. Left
    // in, a count of "how many times did this part change" is inflated several
    // fold. Dedupe on (identifier, date), preferring the substantive row.
    out.amendments
        .sort_by(|a, b| (&a.identifier, &a.date, !a.substantive).cmp(&(&b.identifier, &b.date, !b.substantive)));
    out.amendments
        .dedup_by(|a, b| a.identifier == b.identifier && a.date == b.date);

    // Newest first — "when did this last change?" is the common question.
    out.amendments.sort_by(|a, b| b.date.cmp(&a.date));
    out.citation = Some(citation_of(title, part.as_deref(), req.section.as_deref()));
    out.title = Some(title);
    out.part = part;
    out.section = req.section.clone();
    out.source_url = Some(url);
    if out.amendments.is_empty() {
        out.warnings
            .push("no amendments matched — check --part/--section spelling".to_string());
    }
    Ok(out)
}

async fn structure(title: u32, req: &CfrRequest) -> Result<CfrResponse> {
    let mut out = CfrResponse::empty("structure");
    let date = resolve_date(title, req.date.as_deref(), &mut out.warnings).await?;
    let url = format!("{ECFR_BASE}/versioner/v1/structure/{date}/title-{title}.json");
    let Some(body) = get_text(&url, "ecfr structure", &mut out.warnings).await else {
        out.source_url = Some(url);
        return Ok(out);
    };
    let parsed: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Error::Provider(format!("ecfr structure parse failed: {e}")))?;
    // A whole title's tree is enormous; prune to the requested part when given.
    let pruned = match req
        .part
        .clone()
        .or_else(|| req.section.as_ref().and_then(|s| part_of_section(s)))
    {
        Some(p) => find_node(&parsed, &p).unwrap_or(parsed),
        None => prune_depth(&parsed, 2),
    };
    out.structure = Some(pruned);
    out.title = Some(title);
    out.as_of = Some(date);
    out.source_url = Some(url);
    Ok(out)
}

/// Full-text search across the current CFR.
async fn search_cfr(query: &str, req: &CfrRequest) -> Result<CfrResponse> {
    let mut out = CfrResponse::empty("search");
    let mut url = format!(
        "{ECFR_BASE}/search/v1/results?query={}&per_page=20",
        urlencoding::encode(query)
    );
    if let Some(t) = req.title {
        url.push_str(&format!("&hierarchy%5Btitle%5D={t}"));
    }
    if let Some(p) = req.part.as_deref() {
        url.push_str(&format!("&hierarchy%5Bpart%5D={}", urlencoding::encode(p)));
    }
    if let Some(d) = req.date.as_deref() {
        let d = parse_date(d, "--date")?;
        url.push_str(&format!("&date={d}"));
    }

    let Some(body) = get_text(&url, "ecfr search", &mut out.warnings).await else {
        out.source_url = Some(url);
        return Ok(out);
    };
    let parsed: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| Error::Provider(format!("ecfr search parse failed: {e}")))?;

    if let Some(arr) = parsed.get("results").and_then(|v| v.as_array()) {
        for hit in arr {
            let hierarchy = hit.get("hierarchy");
            let hv = |k: &str| {
                hierarchy
                    .and_then(|x| x.get(k))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            };
            let title_n = hv("title").and_then(|t| t.parse::<u32>().ok());
            let section = hv("section");
            let part = hv("part");
            // Both heading fields come back with <strong> hit-highlighting in
            // them; leaving it in means the model quotes markup back at the user.
            let heading = hit
                .get("headings")
                .and_then(|x| x.get("section"))
                .and_then(|v| v.as_str())
                .map(strip_markup);
            let citation = title_n
                .map(|t| citation_of(t, part.as_deref(), section.as_deref()))
                .unwrap_or_default();
            // eCFR search returns no link field, so build the canonical one.
            let url = match (title_n, section.as_deref()) {
                (Some(t), Some(sec)) => {
                    Some(format!("https://www.ecfr.gov/current/title-{t}/section-{sec}"))
                }
                (Some(t), None) => part
                    .as_deref()
                    .map(|p| format!("https://www.ecfr.gov/current/title-{t}/part-{p}")),
                _ => None,
            };
            out.results.push(CfrSearchHit {
                citation,
                title: title_n,
                part,
                section,
                heading,
                snippet: hit
                    .get("full_text_excerpt")
                    .and_then(|v| v.as_str())
                    .map(strip_markup),
                url,
            });
        }
    }
    out.chars = out.results.len();
    out.source_url = Some(url);
    if out.results.is_empty() {
        out.warnings
            .push(format!("no CFR sections matched {query:?}"));
    }
    Ok(out)
}

/// Fetch the same citation at two dates and report what changed.
async fn diff(title: u32, req: &CfrRequest) -> Result<CfrResponse> {
    // Diff the FULL texts, then clamp the rendered diff. Clamping the inputs
    // first would silently report "no change" for any amendment past the cut.
    const DIFF_INPUT_CAP: usize = 4_000_000;
    let from_req = CfrRequest {
        diff_date: None,
        max_chars: DIFF_INPUT_CAP,
        ..req.clone()
    };
    let to_req = CfrRequest {
        date: req.diff_date.clone(),
        diff_date: None,
        max_chars: DIFF_INPUT_CAP,
        ..req.clone()
    };
    let (from, to) = futures::join!(text(title, &from_req), text(title, &to_req));
    let from = from?;
    let to = to?;

    let a = from.text.clone().unwrap_or_default();
    let b = to.text.clone().unwrap_or_default();

    // The eCFR XML is one long run of text per section; splitting on sentence
    // ends gives a diff a human can actually read.
    let a_lines = sentences(&a);
    let b_lines = sentences(&b);
    let a_refs: Vec<&str> = a_lines.iter().map(String::as_str).collect();
    let b_refs: Vec<&str> = b_lines.iter().map(String::as_str).collect();
    let diff = similar::TextDiff::from_slices(&a_refs, &b_refs);

    let mut rendered = String::new();
    let mut added = 0usize;
    let mut removed = 0usize;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => {
                added += 1;
                rendered.push_str("+ ");
                rendered.push_str(change.value());
                rendered.push('\n');
            }
            similar::ChangeTag::Delete => {
                removed += 1;
                rendered.push_str("- ");
                rendered.push_str(change.value());
                rendered.push('\n');
            }
            similar::ChangeTag::Equal => {}
        }
    }
    let (rendered, truncated) = clamp_text(&rendered, req.max_chars);

    let mut out = CfrResponse::empty("diff");
    out.citation = from.citation.clone().or_else(|| to.citation.clone());
    out.title = Some(title);
    out.part = from.part.clone();
    out.section = req.section.clone();
    out.heading = from.heading.clone().or_else(|| to.heading.clone());
    out.truncated = truncated;
    out.source_url = from.source_url.clone();
    out.warnings.extend(from.warnings);
    out.warnings.extend(to.warnings);
    out.diff = Some(CfrDiff {
        from_date: from.as_of.clone().unwrap_or_default(),
        to_date: to.as_of.clone().unwrap_or_default(),
        changed: added > 0 || removed > 0,
        chars_from: a.chars().count(),
        chars_to: b.chars().count(),
        diff: rendered,
        lines_added: added,
        lines_removed: removed,
    });
    out.as_of = to.as_of;
    Ok(out)
}

// ── helpers ────────────────────────────────────────────────────────────────

async fn get_text(url: &str, source: &str, warnings: &mut Vec<String>) -> Option<String> {
    let client = &*shared_client::BULK;
    let resp = match client.get(url).send().await {
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

/// "240.10b5-1" -> "240"; "1026.19" -> "1026".
fn part_of_section(section: &str) -> Option<String> {
    let head = section.split('.').next()?.trim();
    if head.is_empty() || !head.chars().next()?.is_ascii_digit() {
        return None;
    }
    Some(head.to_string())
}

/// First <HEAD> element — the section heading, before we flatten the markup.
fn first_head(xml: &str) -> Option<String> {
    let start = xml.find("<HEAD>")? + "<HEAD>".len();
    let rest = &xml[start..];
    let end = rest.find("</HEAD>")?;
    let head = strip_markup(&rest[..end]);
    (!head.is_empty()).then_some(head)
}

/// Split regulatory prose into diffable units.
///
/// Naive splitting on any '.' shreds the citations that regulatory text is made
/// of — "§ 240.10b5-1" becomes two units, "15 U.S.C. 78j" becomes four — and a
/// diff of shredded units is unreadable. Break only where a sentence plausibly
/// ends: the period is followed by whitespace, and what precedes it is neither
/// a digit (a section or subsection number) nor a lone initial (U.S.C., No.).
fn sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;

    for i in 0..chars.len() {
        if chars[i] != '.' && chars[i] != ';' {
            continue;
        }
        let next_is_break = chars.get(i + 1).is_none_or(|c| c.is_whitespace());
        if !next_is_break {
            continue;
        }
        let prev = chars[..i].iter().rev().next().copied();
        let is_number = prev.is_some_and(|c| c.is_ascii_digit());
        // A lone capital before the dot is an initial in an abbreviation
        // ("U.S.C.", "F. Supp."), not the end of a sentence.
        let is_initial = prev.is_some_and(|c| c.is_ascii_uppercase())
            && chars[..i]
                .iter()
                .rev()
                .nth(1)
                .is_none_or(|c| !c.is_ascii_alphabetic());
        if chars[i] == '.' && (is_number || is_initial) {
            continue;
        }
        let unit: String = chars[start..=i].iter().collect();
        let trimmed = unit.trim();
        if trimmed.len() > 1 {
            out.push(trimmed.to_string());
            start = i + 1;
        }
    }

    let tail: String = chars[start..].iter().collect();
    let tail = tail.trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

/// Depth-limit the structure tree so a whole title's TOC doesn't blow context.
fn prune_depth(node: &serde_json::Value, depth: usize) -> serde_json::Value {
    let mut copy = node.clone();
    if let Some(obj) = copy.as_object_mut() {
        match obj.get_mut("children") {
            Some(children) if depth == 0 => {
                let n = children.as_array().map(|a| a.len()).unwrap_or(0);
                *children = serde_json::json!(format!("[{n} children elided — narrow with --part]"));
            }
            Some(children) => {
                if let Some(arr) = children.as_array() {
                    let pruned: Vec<_> = arr.iter().map(|c| prune_depth(c, depth - 1)).collect();
                    *children = serde_json::Value::Array(pruned);
                }
            }
            None => {}
        }
    }
    copy
}

/// Find the subtree for one part identifier.
fn find_node(node: &serde_json::Value, identifier: &str) -> Option<serde_json::Value> {
    if node.get("identifier").and_then(|v| v.as_str()) == Some(identifier)
        && node.get("type").and_then(|v| v.as_str()) == Some("part")
    {
        return Some(node.clone());
    }
    node.get("children")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.iter().find_map(|c| find_node(c, identifier)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_part_from_section() {
        assert_eq!(part_of_section("240.10b5-1").as_deref(), Some("240"));
        assert_eq!(part_of_section("1026.19").as_deref(), Some("1026"));
        assert_eq!(part_of_section("Appendix A"), None);
    }

    #[test]
    fn extracts_section_heading() {
        let xml = r#"<DIV8><HEAD>§ 240.10b5-1 Trading "on the basis of".</HEAD><P>text</P></DIV8>"#;
        assert_eq!(
            first_head(xml).as_deref(),
            Some(r#"§ 240.10b5-1 Trading "on the basis of"."#)
        );
    }

    #[test]
    fn strips_highlight_markup_from_search_headings() {
        let raw = "Prevention of misuse of <strong>material</strong> <strong>nonpublic</strong> information.";
        assert_eq!(
            strip_markup(raw),
            "Prevention of misuse of material nonpublic information."
        );
    }

    #[test]
    fn dedupes_reissued_amendments_keeping_the_substantive_row() {
        let mut rows = vec![
            amendment("240.10b5-1", "2023-02-27", false),
            amendment("240.10b5-1", "2023-02-27", true),
            amendment("240.10b5-1", "2022-12-14", true),
            amendment("240.10b-5", "2023-02-27", true),
        ];
        rows.sort_by(|a, b| {
            (&a.identifier, &a.date, !a.substantive).cmp(&(&b.identifier, &b.date, !b.substantive))
        });
        rows.dedup_by(|a, b| a.identifier == b.identifier && a.date == b.date);
        assert_eq!(rows.len(), 3);
        let kept = rows
            .iter()
            .find(|r| r.identifier == "240.10b5-1" && r.date == "2023-02-27")
            .expect("row survives");
        assert!(kept.substantive, "the substantive row is the one kept");
    }

    fn amendment(identifier: &str, date: &str, substantive: bool) -> CfrAmendment {
        CfrAmendment {
            date: date.to_string(),
            issue_date: None,
            identifier: identifier.to_string(),
            name: None,
            part: None,
            subpart: None,
            substantive,
            removed: false,
            node_type: None,
        }
    }

    #[test]
    fn sentence_split_keeps_citations_intact() {
        let text = "Preliminary Note to § 240.10b5-1: This defines trading. \
                    The Act (15 U.S.C. 78j) applies; see also Rule 10b-5.";
        let units = sentences(text);
        assert!(
            units.iter().any(|u| u.contains("§ 240.10b5-1")),
            "a section number must not be split across units: {units:?}"
        );
        assert!(
            units.iter().any(|u| u.contains("15 U.S.C. 78j")),
            "a statutory citation must not be split across units: {units:?}"
        );
        assert!(
            units.iter().any(|u| u.trim_start().starts_with("The Act")),
            "a real sentence boundary must still split: {units:?}"
        );
    }

    #[test]
    fn citation_formatting() {
        assert_eq!(citation_of(17, Some("240"), Some("240.10b5-1")), "17 CFR 240.10b5-1");
        assert_eq!(citation_of(12, Some("1026"), None), "12 CFR 1026");
    }
}
