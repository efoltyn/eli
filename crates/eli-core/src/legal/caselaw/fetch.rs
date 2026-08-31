//! CourtListener case law — search over the corpus, and the text of one opinion.
//!
//! Why this exists when opinions are on the open web: a search engine ranks
//! *pages about* a case, this ranks *the case*. Concretely, three things you
//! cannot ask Google:
//!   * **Boolean, proximity and range search over ~1.1M opinions with an exact
//!     count** — `"border fence"~50`, `citeCount:[100 TO *]`,
//!     `dateFiled:[2018-10-01 TO 2018-10-31]`, scoped to any of 3,359 named
//!     courts. The `count` on a `type=o` search is exact, not "about".
//!   * **Unpublished dispositions as a first-class facet** (`stat_Unpublished`).
//!     Non-precedential orders are largely invisible to web search; here they
//!     are one flag.
//!   * **`citeCount` as a numeric authority signal** — rank or threshold by how
//!     many later decisions rely on a case, rather than by how many blogs
//!     mention it.
//!
//! Access reality (measured, see `courtlistener.rs`): `/search/` answers
//! anonymously; `/opinions/` and `/clusters/` are 401 without a token. So the
//! search half of this module is key-free and the *text* half has to work a
//! ladder of fallbacks, and say which rung it landed on — that is what
//! `text_source` is for. Never let a caller quote text without knowing whether
//! it came from the canonical API record or from a scraped page.

use crate::legal::courtlistener::{self, CL_WEB};
use crate::legal::{clamp_text, parse_date, shared_client, soft_fail, strip_markup};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Search `type` values, with what each one indexes. Kept here rather than in
/// the CLI so the MCP layer gets the same validation and the same error text.
const KINDS: &[(&str, &str)] = &[
    ("o", "case-law opinion clusters"),
    ("r", "RECAP dockets with matching documents"),
    ("rd", "RECAP filing documents"),
    ("d", "RECAP dockets"),
    ("oa", "oral argument audio"),
    ("p", "judges"),
];

/// Precedential-status facets. CourtListener returns *only* `Published` for
/// case-law searches unless you opt in, so an unfiltered search silently hides
/// every unpublished disposition.
const STATUSES: &[&str] = &[
    "Published",
    "Unpublished",
    "Errata",
    "Separate",
    "In-chambers",
    "Relating-to",
    "Unknown",
];

/// Opinion-body source fields on `/opinions/{id}/`, in CourtListener's own
/// recommended read order. `html_with_citations` is the one their site renders:
/// it is derived from whichever source below it was populated, with every
/// detected citation already resolved to a link.
const TEXT_FIELDS: &[&str] = &[
    "html_with_citations",
    "html",
    "xml_harvard",
    "html_columbia",
    "html_lawbox",
    "html_anon_2020",
    "plain_text",
];

#[derive(Clone, Debug)]
pub struct CaseSearchRequest {
    pub query: String,
    pub kind: String,
    pub courts: Vec<String>,
    pub filed_after: Option<String>,
    pub filed_before: Option<String>,
    pub judge: Option<String>,
    pub cited_gt: Option<u32>,
    pub status: Option<String>,
    pub order_by: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CaseHit {
    pub case_name: Option<String>,
    pub citation: Option<String>,
    pub court: Option<String>,
    pub court_id: Option<String>,
    pub date_filed: Option<String>,
    pub docket_number: Option<String>,
    pub judge: Option<String>,
    pub status: Option<String>,
    pub cite_count: Option<u32>,
    pub snippet: Option<String>,
    pub opinion_id: Option<u64>,
    pub cluster_id: Option<u64>,
    pub docket_id: Option<u64>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaseSearchResponse {
    pub generated_at: DateTime<Utc>,
    pub query: String,
    pub kind: String,
    pub total_available: Option<u64>,
    pub results: Vec<CaseHit>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct OpinionRequest {
    pub id: Option<u64>,
    pub cite: Option<String>,
    pub max_chars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpinionResponse {
    pub generated_at: DateTime<Utc>,
    pub opinion_id: Option<u64>,
    pub cluster_id: Option<u64>,
    pub case_name: Option<String>,
    pub citations: Vec<String>,
    pub court: Option<String>,
    pub date_filed: Option<String>,
    pub author: Option<String>,
    pub status: Option<String>,
    pub cite_count: Option<u32>,
    pub text: Option<String>,
    pub chars: usize,
    pub truncated: bool,
    pub url: Option<String>,
    /// Where `text` actually came from. A quote is only as trustworthy as its
    /// provenance, so this is never inferred by the caller:
    /// `courtlistener_api` (canonical record, token), `static.case.law`
    /// (Caselaw Access Project bulk JSON, key-free, coverage ends ~2020),
    /// `courtlistener_web` (scraped HTML page), `search_snippet` (a few hundred
    /// characters of match context, not the opinion), or `none`.
    pub text_source: String,
    /// The exact URL the text was read from, for citation in a brief.
    pub text_source_url: Option<String>,
    pub warnings: Vec<String>,
}

impl OpinionResponse {
    fn empty() -> Self {
        Self {
            generated_at: Utc::now(),
            opinion_id: None,
            cluster_id: None,
            case_name: None,
            citations: Vec::new(),
            court: None,
            date_filed: None,
            author: None,
            status: None,
            cite_count: None,
            text: None,
            chars: 0,
            truncated: false,
            url: None,
            text_source: "none".to_string(),
            text_source_url: None,
            warnings: Vec::new(),
        }
    }

    fn set_text(&mut self, body: &str, source: &str, source_url: Option<String>, max_chars: usize) {
        let (clamped, truncated) = clamp_text(body, max_chars);
        self.chars = body.chars().count();
        self.truncated = truncated;
        self.text = Some(clamped);
        self.text_source = source.to_string();
        self.text_source_url = source_url;
    }
}

// ── search ─────────────────────────────────────────────────────────────────

pub async fn fetch_case_search(req: CaseSearchRequest) -> Result<CaseSearchResponse> {
    let kind = normalize_kind(&req.kind)?;
    let mut warnings: Vec<String> = Vec::new();

    if req.query.trim().is_empty() {
        return Err(Error::InvalidInput(
            "legal search requires --q (boolean AND/OR/NOT, \"phrases\", \"a b\"~50 proximity, \
             field:value, and [x TO y] ranges are all supported)"
                .into(),
        ));
    }

    let limit = req.limit.clamp(1, 100);
    let mut qs: Vec<String> = vec![
        format!("q={}", urlencoding::encode(req.query.trim())),
        format!("type={kind}"),
        // Highlighting is off by default for performance; with it off `snippet`
        // is just the leading 500 characters, which tells the reader nothing
        // about *why* the case matched.
        "highlight=on".to_string(),
    ];

    if !req.courts.is_empty() {
        let courts: Vec<&str> = req
            .courts
            .iter()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .collect();
        if !courts.is_empty() {
            qs.push(format!("court={}", urlencoding::encode(&courts.join(","))));
        }
    }

    // The search backend is Elasticsearch and parses ISO-8601; the website's
    // sidebar sends MM/DD/YYYY but ISO is what was observed working on the API.
    // Validating here means a typo'd date fails with a useful message rather
    // than silently widening the search to everything.
    if let Some(d) = req.filed_after.as_deref() {
        qs.push(format!("filed_after={}", parse_date(d, "--after")?));
    }
    if let Some(d) = req.filed_before.as_deref() {
        qs.push(format!("filed_before={}", parse_date(d, "--before")?));
    }
    if let Some(j) = req.judge.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        qs.push(format!("judge={}", urlencoding::encode(j)));
    }
    if let Some(n) = req.cited_gt {
        qs.push(format!("cited_gt={n}"));
    }
    if let Some(raw) = req.status.as_deref() {
        for facet in status_facets(raw, &mut warnings) {
            qs.push(format!("stat_{facet}=on"));
        }
        if !matches!(kind, "o") {
            warnings.push(format!(
                "--status is a case-law facet and does not apply to type={kind}; it was sent but \
                 will not filter."
            ));
        }
    }
    if let Some(o) = req.order_by.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        qs.push(format!("order_by={}", urlencoding::encode(o)));
    }
    // Default page size is 20. Ask for more only when the caller wants more,
    // so the common case stays one request against a 5/min budget.
    if limit > 20 {
        qs.push(format!("page_size={limit}"));
    }

    let path = format!("search/?{}", qs.join("&"));
    let mut out = CaseSearchResponse {
        generated_at: Utc::now(),
        query: req.query.clone(),
        kind: kind.to_string(),
        total_available: None,
        results: Vec::new(),
        warnings,
    };

    let Some(body) = courtlistener::get(&path, &mut out.warnings).await else {
        return Ok(out);
    };

    out.total_available = body.get("count").and_then(|v| v.as_u64());
    // `type=d`/`type=r` counts come from a cardinality aggregation and drift up
    // to ~6% above a couple of thousand hits. Saying so beats implying precision.
    if matches!(kind, "d" | "r") && out.total_available.is_some_and(|c| c > 2000) {
        out.warnings.push(
            "total_available for docket searches is an approximation (cardinality aggregation, \
             ~6% error above ~2,000 hits)"
                .to_string(),
        );
    }

    if let Some(results) = body.get("results").and_then(|v| v.as_array()) {
        for raw in results.iter().take(limit) {
            out.results.push(parse_hit(raw, kind));
        }
    }

    if out.results.is_empty() && out.warnings.is_empty() {
        out.warnings.push(format!(
            "no {kind} results. Case-law searches return only Published opinions unless you pass \
             --status Unpublished; and a bare -term flips the other terms to fuzzy/OR, so add \
             explicit AND."
        ));
    }
    Ok(out)
}

/// Map a caller's `kind` to a search `type`, accepting the obvious words as
/// well as the API's two-letter codes.
fn normalize_kind(kind: &str) -> Result<&'static str> {
    let k = kind.trim().to_ascii_lowercase();
    let code = match k.as_str() {
        "o" | "opinion" | "opinions" | "case" | "cases" | "caselaw" => "o",
        "r" | "recap" => "r",
        "rd" | "recap-document" | "recap-documents" | "document" | "documents" => "rd",
        "d" | "docket" | "dockets" => "d",
        "oa" | "audio" | "oral" | "oral-argument" => "oa",
        "p" | "person" | "people" | "judge" | "judges" => "p",
        _ => {
            let valid = KINDS
                .iter()
                .map(|(c, what)| format!("{c} ({what})"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::InvalidInput(format!(
                "unknown --kind {kind:?}; valid kinds are {valid}"
            )));
        }
    };
    Ok(code)
}

/// Turn `--status Published,Unpublished` into the `stat_*` facet names.
/// Unknown values are dropped with a warning rather than sent: a bogus facet
/// would be ignored upstream and the caller would think it had filtered.
fn status_facets(raw: &str, warnings: &mut Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let matched = STATUSES
            .iter()
            .find(|s| s.eq_ignore_ascii_case(token) || s.replace('-', " ").eq_ignore_ascii_case(token));
        match matched {
            Some(s) => out.push((*s).to_string()),
            None => warnings.push(format!(
                "ignoring unknown --status {token:?}; valid values are {}",
                STATUSES.join(", ")
            )),
        }
    }
    out
}

fn parse_hit(raw: &serde_json::Value, kind: &str) -> CaseHit {
    let s = |k: &str| {
        raw.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let u = |k: &str| raw.get(k).and_then(|v| v.as_u64());

    // Case-law hits carry the opinion documents in `opinions[]`; RECAP hits
    // carry `recap_documents[]`. Both nest the highlighted snippet one level
    // down, and only `type=rd` puts it at the top level.
    let nested = raw
        .get("opinions")
        .or_else(|| raw.get("recap_documents"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());

    let snippet = nested
        .and_then(|n| n.get("snippet"))
        .or_else(|| raw.get("snippet"))
        .and_then(|v| v.as_str())
        .map(clean_snippet)
        .filter(|t| !t.is_empty());

    let citation = raw
        .get("citation")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .filter(|c| !c.is_empty());

    CaseHit {
        case_name: s("caseName").or_else(|| s("case_name_full")),
        citation,
        court: s("court"),
        court_id: s("court_id"),
        date_filed: s("dateFiled").or_else(|| s("dateArgued")).or_else(|| s("entry_date_filed")),
        docket_number: s("docketNumber"),
        judge: s("judge").or_else(|| s("assignedTo")),
        status: s("status"),
        cite_count: raw.get("citeCount").and_then(|v| v.as_u64()).map(|c| c as u32),
        snippet,
        // For `type=rd` the hit *is* the document, so its own id is the useful one.
        opinion_id: nested
            .and_then(|n| n.get("id"))
            .and_then(|v| v.as_u64())
            .or_else(|| (kind == "rd").then(|| u("id")).flatten()),
        cluster_id: u("cluster_id"),
        docket_id: u("docket_id"),
        url: s("absolute_url").map(|p| absolute(&p)),
    }
}

// ── opinion text ───────────────────────────────────────────────────────────

pub async fn fetch_opinion(req: OpinionRequest) -> Result<OpinionResponse> {
    let mut out = OpinionResponse::empty();
    let max_chars = req.max_chars.max(200);

    let parsed_cite = match req.cite.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => match courtlistener::parse_cite(c) {
            Some(p) => Some(p),
            None => {
                return Err(Error::InvalidInput(format!(
                    "could not read a reporter citation out of {c:?}; expected \
                     \"<volume> <reporter> <page>\", e.g. \"597 U.S. 1\""
                )))
            }
        },
        None => None,
    };
    if req.id.is_none() && parsed_cite.is_none() {
        return Err(Error::InvalidInput(
            "legal opinion requires --id or --cite".into(),
        ));
    }
    out.citations = req.cite.iter().map(|c| c.trim().to_string()).collect();

    // Resolve a citation to an id key-free: `/c/<reporter>/<vol>/<page>/` 302s
    // to the opinion page, whose URL carries the *cluster* id and a slug of the
    // real case name. That name is itself the answer to "is this cite attached
    // to the case you think it is".
    let mut web_url: Option<String> = None;
    if let Some(cite) = parsed_cite.as_ref() {
        let (status, location) = courtlistener::resolve_citation(cite).await;
        match status {
            302 | 301 | 303 | 307 | 308 => {
                if let Some(loc) = location {
                    let (id, name) = courtlistener::parse_opinion_url(&loc);
                    out.cluster_id = id;
                    out.case_name = name;
                    let url = absolute(&loc);
                    out.url = Some(url.clone());
                    web_url = Some(url);
                }
            }
            404 => out.warnings.push(format!(
                "CourtListener has no case at {} {} {} — the citation may be wrong, or the case \
                 may be unpublished or not digitised. Verify with `legal cite` before relying on it.",
                cite.volume, cite.reporter, cite.page
            )),
            0 => out
                .warnings
                .push("citation resolver unreachable; falling back to other sources".to_string()),
            other => out
                .warnings
                .push(format!("citation resolver returned HTTP {other}")),
        }
    }
    if let Some(id) = req.id {
        // Web URLs are cluster-scoped and the API is opinion-scoped, and the two
        // id spaces do not overlap. Say which one we assumed rather than
        // silently returning a different case.
        if courtlistener::has_token() {
            out.opinion_id = Some(id);
        } else {
            out.cluster_id = out.cluster_id.or(Some(id));
            out.warnings.push(format!(
                "--id {id} was read as a CourtListener *cluster* id (the number in an \
                 /opinion/<id>/ web URL). Opinion ids and cluster ids are different id spaces; \
                 set COURTLISTENER_TOKEN to fetch by opinion id."
            ));
        }
        if web_url.is_none() {
            web_url = Some(format!("{CL_WEB}/opinion/{id}/"));
            out.url = out.url.clone().or_else(|| web_url.clone());
        }
    }

    // Rung 1 — the canonical API record. Token only, but it is the only source
    // that carries the citation-linked `html_with_citations` and the cluster
    // metadata (panel, precedential status, citation count) together.
    if courtlistener::has_token() {
        api_text(&mut out, req.id, max_chars).await;
    } else {
        out.warnings.push(
            "no COURTLISTENER_TOKEN: /opinions/ and /clusters/ are 401 without one, so the \
             canonical opinion record is out of reach. Falling back to key-free full-text sources."
                .to_string(),
        );
    }

    // Rung 2 — the Caselaw Access Project static bulk mirror. No key, no rate
    // limit, and it carries the full opinion text plus parallel citations. Its
    // digitisation stops around 2020, so it answers for the historical corpus
    // and misses recent decisions.
    if out.text.is_none() {
        if let Some(cite) = parsed_cite.as_ref() {
            cap_text(&mut out, cite, max_chars).await;
        }
    }

    // Rung 3 — the public opinion page. Free, current, and the only key-free
    // route to a post-2020 decision, but it sits behind a CDN bot challenge
    // that answers non-browser clients with an empty 202.
    if out.text.is_none() {
        if let Some(url) = web_url.as_deref() {
            web_text(&mut out, url, max_chars).await;
        }
    }

    // Rung 4 — metadata (and, failing everything else, the match snippet) from
    // the anonymous search index. One request, and it is the difference between
    // "here is the case" and an empty object.
    if out.case_name.is_none() || out.text.is_none() {
        if let Some(cite) = parsed_cite.as_ref() {
            search_fallback(&mut out, cite, max_chars).await;
        }
    }

    if out.text.is_none() {
        out.warnings.push(
            "no full text available from any key-free source. A free CourtListener token \
             (courtlistener.com/profile/api-token/, set COURTLISTENER_TOKEN) unlocks \
             /opinions/{id}/html_with_citations, which is the authoritative text."
                .to_string(),
        );
    }
    Ok(out)
}

/// Token path: `/opinions/{id}/` for the body, `/clusters/{id}/` for the
/// metadata that lives one level up (case name, date, panel, status).
async fn api_text(out: &mut OpinionResponse, id: Option<u64>, max_chars: usize) {
    let mut cluster_id = out.cluster_id;

    if let Some(oid) = id {
        // `fields=` matters here: the untrimmed opinion record ships every text
        // representation of the same document, several megabytes of it.
        let fields = format!("id,cluster_id,author_str,extracted_by_ocr,{}", TEXT_FIELDS.join(","));
        if let Some(v) = courtlistener::get(
            &format!("opinions/{oid}/?fields={}", urlencoding::encode(&fields)),
            &mut out.warnings,
        )
        .await
        {
            out.opinion_id = v.get("id").and_then(|x| x.as_u64()).or(Some(oid));
            cluster_id = v
                .get("cluster_id")
                .and_then(|x| x.as_u64())
                .or_else(|| {
                    v.get("cluster")
                        .and_then(|x| x.as_str())
                        .and_then(last_path_id)
                })
                .or(cluster_id);
            out.author = v
                .get("author_str")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if v.get("extracted_by_ocr").and_then(|x| x.as_bool()) == Some(true) {
                out.warnings.push(
                    "this opinion's text is OCR output, not a native text layer — treat exact \
                     quotations and numbers with suspicion"
                        .to_string(),
                );
            }
            if let Some((body, field)) = pick_text(&v) {
                let plain = strip_markup(&body);
                out.set_text(
                    &plain,
                    "courtlistener_api",
                    Some(format!(
                        "{}/opinions/{oid}/ ({field})",
                        courtlistener::CL_BASE
                    )),
                    max_chars,
                );
            }
        }
    }

    let Some(cid) = cluster_id else { return };
    out.cluster_id = Some(cid);
    let fields = "id,case_name,case_name_full,date_filed,judges,precedential_status,\
                  citation_count,citations,absolute_url,sub_opinions";
    let Some(v) = courtlistener::get(
        &format!("clusters/{cid}/?fields={}", urlencoding::encode(fields)),
        &mut out.warnings,
    )
    .await
    else {
        return;
    };

    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
    };
    out.case_name = s("case_name").or_else(|| s("case_name_full")).or(out.case_name.take());
    out.date_filed = s("date_filed").or(out.date_filed.take());
    out.author = out.author.take().or_else(|| s("judges"));
    out.status = s("precedential_status").or(out.status.take());
    out.cite_count = v
        .get("citation_count")
        .and_then(|x| x.as_u64())
        .map(|c| c as u32)
        .or(out.cite_count);
    if let Some(arr) = v.get("citations").and_then(|x| x.as_array()) {
        let formatted: Vec<String> = arr.iter().filter_map(format_citation).collect();
        if !formatted.is_empty() {
            out.citations = formatted;
        }
    }
    if let Some(p) = s("absolute_url") {
        out.url = Some(absolute(&p));
    }

    // Still no text: we were given a cite (so a cluster) but no opinion id.
    // The cluster names its opinions; the lead one is the first.
    if out.text.is_none() {
        let sub = v
            .get("sub_opinions")
            .and_then(|x| x.as_array())
            .and_then(|a| a.first())
            .and_then(|x| x.as_str())
            .and_then(last_path_id);
        if let Some(oid) = sub {
            Box::pin(api_text(out, Some(oid), max_chars)).await;
        }
    }
}

/// Caselaw Access Project bulk JSON: `/{reporter}/{volume}/cases/{page}-{n}.json`.
/// The reporter slug is the same punctuation-stripped lowercase form the
/// CourtListener `/c/` resolver uses, and the page is zero-padded to four.
async fn cap_text(out: &mut OpinionResponse, cite: &courtlistener::ParsedCite, max_chars: usize) {
    let Some(url) = cap_case_url(cite) else { return };
    let resp = match shared_client::GENERAL.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            out.warnings.push(format!("static.case.law request failed: {e}"));
            return;
        }
    };
    // A 404 here just means "not in the CAP corpus" — expected for anything
    // decided after their digitisation cut-off. Don't shout about it.
    if resp.status().as_u16() == 404 {
        out.warnings.push(format!(
            "static.case.law has no {} {} {} (its digitisation ends around 2020)",
            cite.volume, cite.reporter, cite.page
        ));
        return;
    }
    let Some(resp) = soft_fail("static.case.law", resp, &mut out.warnings).await else {
        return;
    };
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            out.warnings.push(format!("static.case.law parse failed: {e}"));
            return;
        }
    };

    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
    };
    out.case_name = s("name_abbreviation").or_else(|| s("name")).or(out.case_name.take());
    out.date_filed = s("decision_date").or(out.date_filed.take());
    out.court = v
        .get("court")
        .and_then(|c| c.get("name"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .or(out.court.take());
    if let Some(arr) = v.get("citations").and_then(|x| x.as_array()) {
        let cites: Vec<String> = arr
            .iter()
            .filter_map(|c| c.get("cite").and_then(|x| x.as_str()).map(str::to_string))
            .collect();
        if !cites.is_empty() {
            out.citations = cites;
        }
    }
    out.author = out.author.take().or_else(|| {
        v.get("casebody")
            .and_then(|c| c.get("judges"))
            .and_then(|x| x.as_array())
            .and_then(|a| a.first())
            .and_then(|x| x.as_str())
            .map(str::to_string)
    });

    // A cluster's opinions (majority, concurrence, dissent) are separate
    // entries; concatenating them with their labels keeps a dissent from
    // reading as the holding.
    let body: String = v
        .get("casebody")
        .and_then(|c| c.get("opinions"))
        .and_then(|x| x.as_array())
        .map(|ops| {
            ops.iter()
                .filter_map(|op| {
                    let text = op.get("text").and_then(|t| t.as_str())?;
                    let kind = op.get("type").and_then(|t| t.as_str()).unwrap_or("opinion");
                    Some(format!("[{kind}]\n{text}"))
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    if !body.trim().is_empty() {
        out.set_text(body.trim(), "static.case.law", Some(url), max_chars);
    }
}

/// Scrape the public opinion page. Last key-free resort, and the one most
/// likely to fail: CourtListener fronts its HTML with an AWS WAF that answers
/// a non-browser client with an empty `202` challenge instead of the page.
async fn web_text(out: &mut OpinionResponse, url: &str, max_chars: usize) {
    let resp = match shared_client::GENERAL.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            out.warnings.push(format!("opinion page request failed: {e}"));
            return;
        }
    };
    let challenged = resp.headers().contains_key("x-amzn-waf-action");
    let Some(resp) = soft_fail("courtlistener opinion page", resp, &mut out.warnings).await else {
        return;
    };
    let html = resp.text().await.unwrap_or_default();
    if challenged || html.trim().is_empty() {
        out.warnings.push(format!(
            "the opinion page at {url} is behind a bot challenge (AWS WAF) and returned no \
             content to a programmatic client. Open it in a browser, or set COURTLISTENER_TOKEN \
             to read the text through the API instead."
        ));
        return;
    }
    let Some(body) = extract_opinion_body(&html) else {
        out.warnings
            .push(format!("could not find the opinion body in the page at {url}"));
        return;
    };
    out.set_text(&body, "courtlistener_web", Some(url.to_string()), max_chars);
}

/// One anonymous `/search/` call to fill in whatever the text sources could not.
async fn search_fallback(
    out: &mut OpinionResponse,
    cite: &courtlistener::ParsedCite,
    max_chars: usize,
) {
    let q = format!("\"{} {} {}\"", cite.volume, cite.reporter, cite.page);
    let path = format!(
        "search/?q={}&type=o&highlight=on",
        urlencoding::encode(&q)
    );
    let Some(v) = courtlistener::get(&path, &mut out.warnings).await else {
        return;
    };
    let Some(hit) = v
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
    else {
        return;
    };
    let parsed = parse_hit(hit, "o");
    out.case_name = out.case_name.take().or(parsed.case_name);
    out.court = out.court.take().or(parsed.court);
    out.date_filed = out.date_filed.take().or(parsed.date_filed);
    out.status = out.status.take().or(parsed.status);
    out.cite_count = out.cite_count.or(parsed.cite_count);
    out.cluster_id = out.cluster_id.or(parsed.cluster_id);
    out.opinion_id = out.opinion_id.or(parsed.opinion_id);
    out.url = out.url.take().or(parsed.url);
    if out.citations.len() <= 1 {
        if let Some(c) = parsed.citation {
            out.citations = c.split("; ").map(str::to_string).collect();
        }
    }
    // A snippet is match context, not the opinion — it goes in `text` only
    // because an empty `text` with a populated case name is worse, and
    // `text_source` says exactly what it is.
    if out.text.is_none() {
        if let Some(sn) = parsed.snippet {
            out.set_text(&sn, "search_snippet", out.url.clone(), max_chars);
            out.warnings.push(
                "`text` is the search-result snippet (a few hundred characters of match context), \
                 not the full opinion. Do not quote it as the holding."
                    .to_string(),
            );
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────────────

fn absolute(path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        path.to_string()
    } else {
        format!("{CL_WEB}/{}", path.trim_start_matches('/'))
    }
}

/// `<mark>` highlighting plus HTML entities plus hard-wrapped newlines out of
/// the indexed PDF text; the model wants one readable line.
fn clean_snippet(raw: &str) -> String {
    strip_markup(raw).trim_matches(|c: char| c == '…' || c.is_whitespace()).to_string()
}

/// Pick the best-populated body field, in CourtListener's recommended order,
/// and report which one so the caller can judge OCR risk.
fn pick_text(v: &serde_json::Value) -> Option<(String, &'static str)> {
    TEXT_FIELDS.iter().find_map(|f| {
        v.get(*f)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| (s.to_string(), *f))
    })
}

/// `{volume, reporter, page, type}` -> "410 U.S. 113".
fn format_citation(v: &serde_json::Value) -> Option<String> {
    let vol = v.get("volume")?;
    let rep = v.get("reporter")?.as_str()?;
    let page = v.get("page")?.as_str().map(str::to_string).or_else(|| {
        v.get("page").and_then(|p| p.as_u64()).map(|p| p.to_string())
    })?;
    let vol = vol
        .as_u64()
        .map(|n| n.to_string())
        .or_else(|| vol.as_str().map(str::to_string))?;
    Some(format!("{vol} {rep} {page}"))
}

/// Trailing numeric id out of a hyperlinked resource_uri
/// (".../api/rest/v4/clusters/2812209/" -> 2812209).
fn last_path_id(uri: &str) -> Option<u64> {
    uri.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|s| s.parse::<u64>().ok())
}

/// CAP stores one JSON file per case at `{page:04}-{ordinal}.json`. The first
/// case starting on a page is `-01`, which is the one a citation names.
fn cap_case_url(cite: &courtlistener::ParsedCite) -> Option<String> {
    let page: u32 = cite.page.parse().ok()?;
    let volume: u32 = cite.volume.parse().ok()?;
    let reporter = courtlistener::slug_reporter(&cite.reporter);
    if reporter.is_empty() {
        return None;
    }
    Some(format!(
        "https://static.case.law/{reporter}/{volume}/cases/{page:04}-01.json"
    ))
}

/// Pull the opinion body out of a CourtListener HTML page.
///
/// Kept synchronous and self-contained because `scraper::Html` is not `Send`
/// and must not be held across an await point.
fn extract_opinion_body(html: &str) -> Option<String> {
    // Most specific first: the site has moved this container around over the
    // years and old cached pages still use the older ids.
    const SELECTORS: &[&str] = &[
        "#opinion-content",
        ".opinion-content",
        "#opinion",
        ".serif-text",
        "article",
    ];
    let doc = scraper::Html::parse_document(html);
    for sel in SELECTORS {
        let Ok(selector) = scraper::Selector::parse(sel) else {
            continue;
        };
        if let Some(node) = doc.select(&selector).next() {
            let text = strip_markup(&node.inner_html());
            // A nav shell or an empty container will match the selector and
            // return a line of boilerplate; require enough text to be a decision.
            if text.chars().count() > 400 {
                return Some(text);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_kind_words_and_codes_to_search_types() {
        assert_eq!(normalize_kind("o").unwrap(), "o");
        assert_eq!(normalize_kind("opinions").unwrap(), "o");
        assert_eq!(normalize_kind("RECAP").unwrap(), "r");
        assert_eq!(normalize_kind("rd").unwrap(), "rd");
        assert_eq!(normalize_kind("dockets").unwrap(), "d");
        assert_eq!(normalize_kind(" oral-argument ").unwrap(), "oa");
        assert_eq!(normalize_kind("judges").unwrap(), "p");
    }

    #[test]
    fn unknown_kind_names_the_valid_ones() {
        let err = normalize_kind("briefs").expect_err("should reject");
        let msg = err.to_string();
        assert!(msg.contains("briefs"), "{msg}");
        for (code, _) in KINDS {
            assert!(msg.contains(&format!("{code} (")), "missing {code} in {msg}");
        }
    }

    #[test]
    fn status_facets_normalize_and_reject() {
        let mut w = Vec::new();
        assert_eq!(
            status_facets("published,UNPUBLISHED,in chambers", &mut w),
            vec!["Published", "Unpublished", "In-chambers"]
        );
        assert!(w.is_empty());

        let mut w2 = Vec::new();
        assert!(status_facets("precedential", &mut w2).is_empty());
        assert!(w2[0].contains("Unpublished"), "{:?}", w2);
    }

    #[test]
    fn snippet_loses_its_highlight_markup() {
        let raw = "their actions were protected by\n<mark>qualified immunity</mark>. The \
                   district court also found&nbsp;no ret…";
        let cleaned = clean_snippet(raw);
        assert!(!cleaned.contains("<mark>"), "{cleaned}");
        assert!(cleaned.contains("qualified immunity"), "{cleaned}");
        // entity decoded, newline collapsed, trailing ellipsis trimmed
        assert!(cleaned.contains("also found no ret"), "{cleaned}");
        assert!(!cleaned.contains('\n'), "{cleaned}");
    }

    #[test]
    fn builds_absolute_urls_from_relative_paths() {
        assert_eq!(
            absolute("/opinion/10958441/grenning-v-key/"),
            "https://www.courtlistener.com/opinion/10958441/grenning-v-key/"
        );
        assert_eq!(
            absolute("https://www.courtlistener.com/opinion/1/x/"),
            "https://www.courtlistener.com/opinion/1/x/"
        );
    }

    #[test]
    fn parses_a_search_hit_into_a_case() {
        let raw = serde_json::json!({
            "caseName": "Grenning v. Key",
            "court": "Court of Appeals for the Ninth Circuit",
            "court_id": "ca9",
            "dateFiled": "2026-08-26",
            "docketNumber": "23-3018",
            "cluster_id": 10958441,
            "docket_id": 73108335,
            "citation": ["612 F.3d 1099", "2010 WL 1"],
            "citeCount": 7,
            "status": "Published",
            "absolute_url": "/opinion/10958441/grenning-v-key/",
            "opinions": [{"id": 11426046, "snippet": "protected by <mark>qualified immunity</mark>."}]
        });
        let hit = parse_hit(&raw, "o");
        assert_eq!(hit.case_name.as_deref(), Some("Grenning v. Key"));
        assert_eq!(hit.opinion_id, Some(11426046));
        assert_eq!(hit.cluster_id, Some(10958441));
        assert_eq!(hit.cite_count, Some(7));
        assert_eq!(hit.citation.as_deref(), Some("612 F.3d 1099; 2010 WL 1"));
        assert_eq!(
            hit.url.as_deref(),
            Some("https://www.courtlistener.com/opinion/10958441/grenning-v-key/")
        );
        let snippet = hit.snippet.expect("snippet");
        assert!(!snippet.contains("<mark>"), "{snippet}");
        assert!(snippet.starts_with("protected by qualified immunity"), "{snippet}");
    }

    #[test]
    fn cap_urls_zero_pad_the_page() {
        let cite = courtlistener::parse_cite("410 U.S. 113").expect("parse");
        assert_eq!(
            cap_case_url(&cite).as_deref(),
            Some("https://static.case.law/us/410/cases/0113-01.json")
        );
        let cite = courtlistener::parse_cite("612 F.3d 1099").expect("parse");
        assert_eq!(
            cap_case_url(&cite).as_deref(),
            Some("https://static.case.law/f3d/612/cases/1099-01.json")
        );
    }

    #[test]
    fn text_fields_are_read_in_courtlisteners_priority_order() {
        let v = serde_json::json!({"plain_text": "raw", "html_with_citations": "<p>linked</p>"});
        assert_eq!(
            pick_text(&v),
            Some(("<p>linked</p>".to_string(), "html_with_citations"))
        );
        let v = serde_json::json!({"html_with_citations": "  ", "plain_text": "raw"});
        assert_eq!(pick_text(&v), Some(("raw".to_string(), "plain_text")));
        assert_eq!(pick_text(&serde_json::json!({})), None);
    }

    #[test]
    fn formats_structured_citations() {
        assert_eq!(
            format_citation(&serde_json::json!({"volume": 410, "reporter": "U.S.", "page": "113"}))
                .as_deref(),
            Some("410 U.S. 113")
        );
    }

    #[test]
    fn reads_ids_out_of_hyperlinked_resource_uris() {
        assert_eq!(
            last_path_id("https://www.courtlistener.com/api/rest/v4/clusters/2812209/"),
            Some(2812209)
        );
        assert_eq!(last_path_id("not-a-uri"), None);
    }

    #[test]
    fn opinion_body_extraction_ignores_boilerplate_containers() {
        let filler = "The judgment of the district court is affirmed. ".repeat(20);
        let html = format!(
            "<html><body><article>nav</article><div id=\"opinion-content\"><p>{filler}</p></div></body></html>"
        );
        let body = extract_opinion_body(&html).expect("body");
        assert!(body.starts_with("The judgment"), "{body}");
        assert!(extract_opinion_body("<html><body><article>nav</article></body></html>").is_none());
    }
}
