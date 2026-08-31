//! Federal Register — the daily journal of US agency action.
//!
//! Three things here are hard to get any other way:
//!   * the **public-inspection queue** — documents filed and legally effective
//!     on filing, but not published until tomorrow. Nothing has indexed them
//!     yet, by construction.
//!   * **structured rule metadata** — comment deadline, effective date, RIN,
//!     regulations.gov docket, and the exact CFR parts a rule amends, as
//!     fields rather than prose to be re-read out of a PDF.
//!   * **facet counts over time** — "how much has this agency written about X
//!     per month", which is a count over the whole corpus, not a page anyone
//!     has published.
//!
//! No API key. Base: https://www.federalregister.gov/api/v1/

use crate::legal::{clamp_text, parse_date, shared_client, soft_fail};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const FR_BASE: &str = "https://www.federalregister.gov/api/v1";

/// Requested explicitly so the API doesn't hand back its short default set.
const FR_FIELDS: &[&str] = &[
    "document_number",
    "title",
    "type",
    "abstract",
    "publication_date",
    "effective_on",
    "comments_close_on",
    "docket_ids",
    "regulation_id_numbers",
    "cfr_references",
    "significant",
    "agencies",
    "html_url",
    "raw_text_url",
    "action",
    "dates",
    "page_length",
    "regulations_dot_gov_info",
];

#[derive(Clone, Debug)]
pub struct FedregRequest {
    pub query: Option<String>,
    pub kind: Option<String>,
    pub agencies: Vec<String>,
    pub published_after: Option<String>,
    pub published_before: Option<String>,
    pub docket: Option<String>,
    pub cfr_title: Option<u32>,
    pub cfr_part: Option<String>,
    pub document_number: Option<String>,
    pub with_text: bool,
    pub public_inspection: bool,
    pub facet: Option<String>,
    pub max_chars: usize,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FedregDocument {
    pub document_number: Option<String>,
    pub title: Option<String>,
    pub doc_type: Option<String>,
    pub agencies: Vec<String>,
    pub publication_date: Option<String>,
    /// When the rule bites. Absent for most notices.
    pub effective_on: Option<String>,
    /// The comment deadline — the single most time-critical field here.
    pub comments_close_on: Option<String>,
    pub docket_ids: Vec<String>,
    pub regulation_id_numbers: Vec<String>,
    /// CFR parts this document amends, as "17 CFR 240".
    pub cfr_references: Vec<String>,
    pub significant: Option<bool>,
    pub action: Option<String>,
    pub page_length: Option<u64>,
    /// Only set on the public-inspection queue: when it was filed.
    pub filed_at: Option<String>,
    /// The regulations.gov document id for this rule — the bridge from a
    /// Federal Register document to its comment record, which is otherwise a
    /// guess. Feed it (or the docket id) to legal_comments.
    pub regulations_gov_document_id: Option<String>,
    pub abstract_text: Option<String>,
    pub html_url: Option<String>,
    pub raw_text_url: Option<String>,
    pub text: Option<String>,
    pub text_truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FedregFacet {
    pub key: String,
    pub name: Option<String>,
    pub count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FedregResponse {
    pub generated_at: DateTime<Utc>,
    /// "search" | "document" | "public_inspection" | "facet"
    pub mode: String,
    pub query: Option<String>,
    /// Total matches upstream, which is usually far more than `documents.len()`.
    pub total_available: Option<u64>,
    pub documents: Vec<FedregDocument>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub facets: Vec<FedregFacet>,
    pub source_url: Option<String>,
    pub warnings: Vec<String>,
}

impl FedregResponse {
    fn empty(mode: &str) -> Self {
        Self {
            generated_at: Utc::now(),
            mode: mode.to_string(),
            query: None,
            total_available: None,
            documents: Vec::new(),
            facets: Vec::new(),
            source_url: None,
            warnings: Vec::new(),
        }
    }
}

pub async fn fetch_fedreg(req: FedregRequest) -> Result<FedregResponse> {
    if let Some(doc) = req.document_number.clone() {
        return single_document(&doc, &req).await;
    }
    if req.public_inspection {
        return public_inspection(&req).await;
    }
    if let Some(facet) = req.facet.clone() {
        return facets(&facet, &req).await;
    }
    search(&req).await
}

/// Normalize the friendly type names to the API's codes.
fn doc_type_code(kind: &str) -> Option<&'static str> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "rule" | "final" | "final_rule" => Some("RULE"),
        "proposed" | "proposed_rule" | "prorule" | "nprm" => Some("PRORULE"),
        "notice" => Some("NOTICE"),
        "presidential" | "presdocu" | "executive_order" | "eo" => Some("PRESDOCU"),
        _ => None,
    }
}

fn build_conditions(req: &FedregRequest, out: &mut Vec<String>) -> Result<()> {
    if let Some(q) = req.query.as_deref() {
        out.push(format!(
            "conditions%5Bterm%5D={}",
            urlencoding::encode(q)
        ));
    }
    if let Some(kind) = req.kind.as_deref() {
        match doc_type_code(kind) {
            Some(code) => out.push(format!("conditions%5Btype%5D%5B%5D={code}")),
            None => {
                return Err(Error::InvalidInput(format!(
                    "unknown --kind {kind:?} (rule|proposed|notice|presidential)"
                )))
            }
        }
    }
    for agency in &req.agencies {
        out.push(format!(
            "conditions%5Bagencies%5D%5B%5D={}",
            urlencoding::encode(agency)
        ));
    }
    if let Some(after) = req.published_after.as_deref() {
        let d = parse_date(after, "--after")?;
        out.push(format!("conditions%5Bpublication_date%5D%5Bgte%5D={d}"));
    }
    if let Some(before) = req.published_before.as_deref() {
        let d = parse_date(before, "--before")?;
        out.push(format!("conditions%5Bpublication_date%5D%5Blte%5D={d}"));
    }
    if let Some(docket) = req.docket.as_deref() {
        out.push(format!(
            "conditions%5Bdocket_id%5D={}",
            urlencoding::encode(docket)
        ));
    }
    if let Some(t) = req.cfr_title {
        out.push(format!("conditions%5Bcfr%5D%5Btitle%5D={t}"));
    }
    if let Some(p) = req.cfr_part.as_deref() {
        out.push(format!(
            "conditions%5Bcfr%5D%5Bpart%5D={}",
            urlencoding::encode(p)
        ));
    }
    Ok(())
}

async fn search(req: &FedregRequest) -> Result<FedregResponse> {
    let mut out = FedregResponse::empty("search");
    let mut qs: Vec<String> = FR_FIELDS
        .iter()
        .map(|f| format!("fields%5B%5D={f}"))
        .collect();
    qs.push(format!("per_page={}", req.limit.clamp(1, 100)));
    qs.push("order=newest".to_string());
    build_conditions(req, &mut qs)?;

    if req.query.is_none()
        && req.agencies.is_empty()
        && req.docket.is_none()
        && req.cfr_title.is_none()
    {
        out.warnings.push(
            "no query or filter given — returning the most recent documents across all agencies"
                .to_string(),
        );
    }

    let url = format!("{FR_BASE}/documents.json?{}", qs.join("&"));
    let Some(body) = get_json(&url, "federal register", &mut out.warnings).await else {
        out.source_url = Some(url);
        return Ok(out);
    };

    out.total_available = body.get("count").and_then(|v| v.as_u64());
    if let Some(arr) = body.get("results").and_then(|v| v.as_array()) {
        for item in arr {
            out.documents.push(parse_document(item));
        }
    }
    out.query = req.query.clone();
    out.source_url = Some(url);

    if req.with_text {
        hydrate_text(&mut out, req.max_chars).await;
    }
    if out.documents.is_empty() {
        out.warnings
            .push("no Federal Register documents matched".to_string());
    }
    Ok(out)
}

async fn single_document(doc: &str, req: &FedregRequest) -> Result<FedregResponse> {
    let mut out = FedregResponse::empty("document");
    let fields = FR_FIELDS
        .iter()
        .map(|f| format!("fields%5B%5D={f}"))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("{FR_BASE}/documents/{}.json?{fields}", urlencoding::encode(doc));
    let Some(body) = get_json(&url, "federal register document", &mut out.warnings).await else {
        out.source_url = Some(url);
        return Ok(out);
    };
    out.documents.push(parse_document(&body));
    out.total_available = Some(1);
    out.source_url = Some(url);
    // A single-document lookup almost always wants the text, so default it on.
    hydrate_text(&mut out, req.max_chars).await;
    Ok(out)
}

/// Documents filed for public inspection — tomorrow's Federal Register, today.
async fn public_inspection(req: &FedregRequest) -> Result<FedregResponse> {
    let mut out = FedregResponse::empty("public_inspection");
    let url = format!("{FR_BASE}/public-inspection-documents/current.json");
    let Some(body) = get_json(&url, "public inspection", &mut out.warnings).await else {
        out.source_url = Some(url);
        return Ok(out);
    };

    out.total_available = body.get("count").and_then(|v| v.as_u64());
    let want = req.query.as_deref().map(str::to_lowercase);
    let want_type = req.kind.as_deref().and_then(doc_type_code);

    if let Some(arr) = body.get("results").and_then(|v| v.as_array()) {
        for item in arr {
            let doc = parse_document(item);
            // This endpoint takes no filters, so filter client-side.
            if let Some(needle) = want.as_deref() {
                let hay = format!(
                    "{} {} {}",
                    doc.title.clone().unwrap_or_default(),
                    doc.abstract_text.clone().unwrap_or_default(),
                    doc.agencies.join(" ")
                )
                .to_lowercase();
                if !hay.contains(needle) {
                    continue;
                }
            }
            if let Some(t) = want_type {
                let matches = doc
                    .doc_type
                    .as_deref()
                    .map(|d| doc_type_code(d).map(|c| c == t).unwrap_or(false))
                    .unwrap_or(false);
                if !matches {
                    continue;
                }
            }
            out.documents.push(doc);
            if out.documents.len() >= req.limit {
                break;
            }
        }
    }
    out.query = req.query.clone();
    out.source_url = Some(url);
    out.warnings.push(
        "public-inspection documents are legally filed but not yet published; the publication_date \
         field is the issue they will appear in"
            .to_string(),
    );
    if req.with_text {
        hydrate_text(&mut out, req.max_chars).await;
    }
    Ok(out)
}

/// Counts over the whole corpus, bucketed. `daily|weekly|monthly|quarterly|yearly|agency|type|topic|section`.
async fn facets(facet: &str, req: &FedregRequest) -> Result<FedregResponse> {
    let mut out = FedregResponse::empty("facet");
    let facet = facet.trim().to_ascii_lowercase();
    let mut qs: Vec<String> = Vec::new();
    build_conditions(req, &mut qs)?;
    let url = format!("{FR_BASE}/documents/facets/{facet}?{}", qs.join("&"));
    let Some(body) = get_json(&url, "federal register facets", &mut out.warnings).await else {
        out.source_url = Some(url);
        return Ok(out);
    };

    // Facet responses are a flat object keyed by bucket.
    if let Some(obj) = body.as_object() {
        for (key, val) in obj {
            out.facets.push(FedregFacet {
                key: key.clone(),
                name: val.get("name").and_then(|v| v.as_str()).map(str::to_string),
                count: val.get("count").and_then(|v| v.as_u64()).unwrap_or(0),
            });
        }
    }
    out.facets.sort_by(|a, b| a.key.cmp(&b.key));
    out.total_available = Some(out.facets.iter().map(|f| f.count).sum());
    out.query = req.query.clone();
    out.source_url = Some(url);
    if out.facets.is_empty() {
        out.warnings.push(format!(
            "no counts for facet {facet:?} — valid facets are daily, weekly, monthly, quarterly, \
             yearly, agency, type, topic, section"
        ));
    }
    Ok(out)
}

/// Pull each document's plain text from `raw_text_url`.
///
/// Sequential on purpose: this is GPO's server and a burst of parallel fetches
/// on a 100-document page is rude enough to get an IP throttled.
async fn hydrate_text(out: &mut FedregResponse, max_chars: usize) {
    let client = &*shared_client::GENERAL;
    for doc in out.documents.iter_mut() {
        let Some(url) = doc.raw_text_url.clone() else {
            continue;
        };
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(t) => match unwrap_raw_text(&t) {
                    Some(body) => {
                        let (clamped, truncated) = clamp_text(&body, max_chars);
                        doc.text = Some(clamped);
                        doc.text_truncated = truncated;
                    }
                    None => out.warnings.push(format!(
                        "full text for {} came back as an access-wall page, not the document —                          read html_url directly",
                        doc.document_number.clone().unwrap_or_default()
                    )),
                },
                Err(e) => out.warnings.push(format!("text read failed for {url}: {e}")),
            },
            Ok(resp) => out
                .warnings
                .push(format!("text fetch {} for {url}", resp.status())),
            Err(e) => out.warnings.push(format!("text fetch failed for {url}: {e}")),
        }
    }
}

/// The Federal Register's "raw text" is really plain text inside a
/// `<html><body><pre>` wrapper. Unwrap it, and recognize the bot-challenge
/// page it serves instead when it doesn't like the request — returning that
/// HTML as if it were the rule would be worse than returning nothing.
fn unwrap_raw_text(body: &str) -> Option<String> {
    if let (Some(start), Some(end)) = (body.find("<pre>"), body.rfind("</pre>")) {
        if end > start {
            let inner = &body[start + "<pre>".len()..end];
            return Some(html_escape::decode_html_entities(inner).trim().to_string());
        }
    }
    let looks_like_html = body.trim_start().starts_with("<!DOCTYPE")
        || body.trim_start().starts_with("<html");
    if looks_like_html {
        return None;
    }
    Some(body.trim().to_string())
}

fn parse_document(item: &serde_json::Value) -> FedregDocument {
    let str_field = |k: &str| item.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let str_list = |k: &str| {
        item.get(k)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };

    let agencies = item
        .get("agencies")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    x.get("name")
                        .or_else(|| x.get("raw_name"))
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    // cfr_references arrive as {title, part, chapter}, where `part` is a STRING
    // ("230") or null. Flatten to "17 CFR 230" and dedupe — a rule amending six
    // parts of one title otherwise repeats the title six times.
    let mut cfr_references: Vec<String> = item
        .get("cfr_references")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let title = x.get("title").and_then(|t| {
                        t.as_u64()
                            .map(|n| n.to_string())
                            .or_else(|| t.as_str().map(str::to_string))
                    })?;
                    let part = x.get("part").and_then(|p| {
                        p.as_str()
                            .map(str::to_string)
                            .or_else(|| p.as_u64().map(|n| n.to_string()))
                    });
                    Some(match part {
                        Some(part) => format!("{title} CFR {part}"),
                        None => format!("{title} CFR"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    cfr_references.dedup();

    FedregDocument {
        document_number: str_field("document_number"),
        title: str_field("title").map(|t| t.trim().to_string()),
        doc_type: str_field("type"),
        agencies,
        publication_date: str_field("publication_date"),
        effective_on: str_field("effective_on"),
        comments_close_on: str_field("comments_close_on"),
        docket_ids: str_list("docket_ids"),
        regulation_id_numbers: str_list("regulation_id_numbers"),
        cfr_references,
        significant: item.get("significant").and_then(|v| v.as_bool()),
        action: str_field("action"),
        page_length: item.get("page_length").and_then(|v| v.as_u64()),
        filed_at: str_field("filed_at"),
        regulations_gov_document_id: item
            .get("regulations_dot_gov_info")
            .and_then(|r| r.get("document_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        abstract_text: str_field("abstract"),
        html_url: str_field("html_url"),
        raw_text_url: str_field("raw_text_url"),
        text: None,
        text_truncated: false,
    }
}

async fn get_json(
    url: &str,
    source: &str,
    warnings: &mut Vec<String>,
) -> Option<serde_json::Value> {
    let client = &*shared_client::GENERAL;
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("{source} request failed: {e}"));
            return None;
        }
    };
    let resp = soft_fail(source, resp, warnings).await?;
    match resp.json::<serde_json::Value>().await {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("{source} parse failed: {e}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> FedregRequest {
        FedregRequest {
            query: None,
            kind: None,
            agencies: Vec::new(),
            published_after: None,
            published_before: None,
            docket: None,
            cfr_title: None,
            cfr_part: None,
            document_number: None,
            with_text: false,
            public_inspection: false,
            facet: None,
            max_chars: 1000,
            limit: 20,
        }
    }

    #[test]
    fn unwraps_pre_block_and_rejects_access_wall() {
        let wrapped = "<html><head></head><body><pre>\n[Federal Register Volume 91]\nRule text &amp; more\n</pre></body></html>";
        assert_eq!(
            unwrap_raw_text(wrapped).as_deref(),
            Some("[Federal Register Volume 91]\nRule text & more")
        );
        assert_eq!(unwrap_raw_text("<!DOCTYPE html>\n<html><body>Request Access</body></html>"), None);
        assert_eq!(unwrap_raw_text("plain rule text").as_deref(), Some("plain rule text"));
    }

    #[test]
    fn maps_friendly_type_names() {
        assert_eq!(doc_type_code("proposed"), Some("PRORULE"));
        assert_eq!(doc_type_code("NPRM"), Some("PRORULE"));
        assert_eq!(doc_type_code("rule"), Some("RULE"));
        assert_eq!(doc_type_code("nonsense"), None);
    }

    #[test]
    fn rejects_unknown_type() {
        let mut qs = Vec::new();
        let r = FedregRequest {
            kind: Some("nonsense".into()),
            ..req()
        };
        assert!(build_conditions(&r, &mut qs).is_err());
    }

    #[test]
    fn builds_cfr_and_date_conditions() {
        let mut qs = Vec::new();
        let r = FedregRequest {
            query: Some("insider trading".into()),
            cfr_title: Some(17),
            cfr_part: Some("240".into()),
            published_after: Some("2024-01-01".into()),
            ..req()
        };
        build_conditions(&r, &mut qs).expect("conditions");
        let joined = qs.join("&");
        assert!(joined.contains("conditions%5Bterm%5D=insider%20trading"));
        assert!(joined.contains("conditions%5Bcfr%5D%5Btitle%5D=17"));
        assert!(joined.contains("conditions%5Bpublication_date%5D%5Bgte%5D=2024-01-01"));
    }

    #[test]
    fn extracts_the_regulations_gov_bridge_id() {
        let raw = serde_json::json!({
            "document_number": "2026-17183",
            "regulations_dot_gov_info": {"document_id": "SEC-2026-5190-0001", "comment_url": null}
        });
        let doc = parse_document(&raw);
        assert_eq!(
            doc.regulations_gov_document_id.as_deref(),
            Some("SEC-2026-5190-0001")
        );
    }

    #[test]
    fn dedupes_repeated_cfr_titles() {
        let raw = serde_json::json!({
            "cfr_references": [
                {"title": 17, "part": "230"},
                {"title": 17, "part": "240"},
                {"title": 17, "part": null}
            ]
        });
        let doc = parse_document(&raw);
        assert_eq!(doc.cfr_references, vec!["17 CFR 230", "17 CFR 240", "17 CFR"]);
    }

    #[test]
    fn flattens_agency_and_cfr_shapes() {
        let raw = serde_json::json!({
            "document_number": "2024-12345",
            "title": " A Rule ",
            "type": "Rule",
            "agencies": [{"name": "Securities and Exchange Commission"}],
            "cfr_references": [{"title": 17, "part": "240"}],
            "docket_ids": ["S7-20-22"],
            "comments_close_on": "2024-03-01"
        });
        let doc = parse_document(&raw);
        assert_eq!(doc.title.as_deref(), Some("A Rule"));
        assert_eq!(doc.agencies, vec!["Securities and Exchange Commission"]);
        assert_eq!(doc.cfr_references, vec!["17 CFR 240"]);
        assert_eq!(doc.comments_close_on.as_deref(), Some("2024-03-01"));
    }
}
