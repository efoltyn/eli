//! regulations.gov — rulemaking dockets and the public comments filed on them.
//!
//! Why this exists when the whole docket is nominally "on the web": a comment
//! lives at a `regulations.gov/comment/{id}` route that is rendered
//! client-side out of this same JSON:API, and its actual argument is usually a
//! PDF attachment on a separate `fileUrl`. There is no crawlable HTML page
//! carrying the text, and a busy docket has tens of thousands of them — no
//! crawler budget covers that, so a search engine has effectively never seen
//! them. They are also the only public record of *who objected and why* before
//! the agency answers: the final rule's preamble paraphrases and rebuts
//! comments months later, but the comments themselves are the primary source,
//! timestamped and attributed to a named bank, exchange, or trade group.
//!
//! **Key-gated.** regulations.gov v4 sits behind api.data.gov; a missing key is
//! a 403 `API_KEY_MISSING`, and `DEMO_KEY` is a globally-shared 10 req/hr
//! bucket that is permanently exhausted, so it is not a fallback. What *is*
//! key-free is the Federal Register, which publishes
//! `regulations_dot_gov_info.document_id` for each rule — the bridge back to a
//! regulations.gov docket. So without a key we still hand back the rulemaking
//! record (the rule, its comment deadline, its comment count, and the docket
//! URL); we just cannot hand back the comments. That degradation is stated in
//! `warnings` rather than returned as an empty list.
//!
//! Bases: https://api.regulations.gov/v4 and
//! https://www.federalregister.gov/api/v1

use crate::legal::{clamp_text, keys, parse_date, shared_client, soft_fail, strip_markup};
use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const REGS_BASE: &str = "https://api.regulations.gov/v4";
const FR_BASE: &str = "https://www.federalregister.gov/api/v1";

/// Measured against the live API: `page[size]=2` is rejected with
/// "Page size parameter must be a positive number of 5 or greater." We clamp
/// here and trim client-side so a `--limit 3` still answers.
const REGS_PAGE_MIN: usize = 5;
const REGS_PAGE_MAX: usize = 250;

/// A single comment body can be a hundred pages of pasted text. Cap it so one
/// call can't blow the context window; the attachment URLs are still returned.
const COMMENT_TEXT_CAP: usize = 12_000;

/// How many distinct commentable documents in a docket we chase comments for.
/// A docket usually has one (`-0001`), occasionally a re-opening notice; past
/// three we are spending requests on supporting material nobody commented on.
const MAX_COMMENT_TARGETS: usize = 3;

#[derive(Clone, Debug)]
pub struct CommentsRequest {
    pub docket: Option<String>,
    pub query: Option<String>,
    pub agency: Option<String>,
    pub posted_after: Option<String>,
    pub comment_id: Option<String>,
    pub with_text: bool,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CommentRecord {
    pub id: Option<String>,
    pub tracking_number: Option<String>,
    pub title: Option<String>,
    pub submitter: Option<String>,
    pub organization: Option<String>,
    pub posted_date: Option<String>,
    pub docket_id: Option<String>,
    pub agency_id: Option<String>,
    pub comment_text: Option<String>,
    pub attachment_urls: Vec<String>,
    pub url: Option<String>,
    /// What this row actually is. `"comment"` is a real public comment from
    /// regulations.gov; `"rulemaking_document"` is the key-free Federal
    /// Register fallback — the rule being commented *on*, not a comment. The
    /// distinction matters enough to be a field rather than a footnote: a
    /// caller must never read the agency's own notice as public opposition.
    #[serde(default)]
    pub record_type: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommentsResponse {
    pub generated_at: DateTime<Utc>,
    pub docket: Option<String>,
    /// Upstream's own total, which is normally far larger than `comments.len()`.
    /// In the no-key path this is the agency-reported comment count for the
    /// docket — i.e. how many comments you are *not* seeing.
    pub total_available: Option<u64>,
    pub comments: Vec<CommentRecord>,
    pub has_api_key: bool,
    pub source_url: Option<String>,
    pub warnings: Vec<String>,
}

impl CommentsResponse {
    fn empty(docket: Option<String>) -> Self {
        Self {
            generated_at: Utc::now(),
            docket,
            total_available: None,
            comments: Vec::new(),
            has_api_key: false,
            source_url: None,
            warnings: Vec::new(),
        }
    }
}

pub async fn fetch_comments(req: CommentsRequest) -> Result<CommentsResponse> {
    let mut out = CommentsResponse::empty(req.docket.clone());
    // Validate the date before spending a request on it.
    if let Some(d) = req.posted_after.as_deref() {
        parse_date(d, "--after")?;
    }

    match keys::data_gov() {
        Some(key) => {
            out.has_api_key = true;
            with_key(&req, &key, &mut out).await;
        }
        None => {
            out.has_api_key = false;
            out.warnings.push(no_key_message());
            without_key(&req, &mut out).await;
        }
    }
    Ok(out)
}

fn no_key_message() -> String {
    "no regulations.gov API key: public comments are unreachable. Get a free key at \
     https://api.data.gov/signup/ (instant, 1,000 req/hr) and set REGULATIONS_GOV_API_KEY \
     (DATA_GOV_API_KEY / GOVINFO_API_KEY / API_DATA_GOV_KEY are also read). DEMO_KEY is not a \
     workaround — it is a 10 req/hr bucket shared by every anonymous caller on the internet and \
     is permanently exhausted. Falling back to the key-free Federal Register rulemaking record: \
     you get the rule, its comment deadline and the agency's comment count, but not the comments."
        .to_string()
}

// ── keyed path: regulations.gov v4 ─────────────────────────────────────────

async fn with_key(req: &CommentsRequest, key: &str, out: &mut CommentsResponse) {
    // A single comment by id is the only way to get the full body plus
    // attachments — the list endpoint truncates and omits the submitter.
    if let Some(id) = req.comment_id.as_deref() {
        let url = format!("{REGS_BASE}/comments/{id}?include=attachments");
        out.source_url = Some(url.clone());
        if let Some(v) = regs_get(&url, key, "regulations.gov comment", &mut out.warnings).await {
            if let Some(rec) = detail_to_record(&v) {
                out.comments.push(rec);
            } else {
                out.warnings
                    .push(format!("regulations.gov: comment {id} returned no data"));
            }
        }
        return;
    }

    // Comments are keyed to a document's internal `objectId`, never to the
    // human-facing docket id, so a docket query is two hops: list the docket's
    // documents, read their objectIds, then list comments on those.
    let mut object_ids: Vec<(String, String)> = Vec::new();
    if let Some(docket) = req.docket.as_deref() {
        object_ids = docket_comment_targets(docket, key, out).await;
        if object_ids.is_empty() {
            out.warnings.push(format!(
                "regulations.gov: docket {docket} has no document with a commentable objectId — \
                 check the docket id, or search with --q instead"
            ));
            return;
        }
    }

    let page_size = req.limit.clamp(REGS_PAGE_MIN, REGS_PAGE_MAX);
    if req.limit < REGS_PAGE_MIN {
        out.warnings.push(format!(
            "regulations.gov enforces a minimum page[size] of {REGS_PAGE_MIN}; fetched {REGS_PAGE_MIN} \
             and trimmed to {}",
            req.limit
        ));
    }

    let mut urls: Vec<String> = Vec::new();
    if object_ids.is_empty() {
        urls.push(comments_url(req, None, page_size, key.len()));
    } else {
        for (_doc_id, object_id) in object_ids.iter().take(MAX_COMMENT_TARGETS) {
            urls.push(comments_url(req, Some(object_id), page_size, key.len()));
        }
    }
    out.source_url = urls.first().cloned();

    // Independent lists — fan out rather than walking them in series.
    let fetched = futures::future::join_all(
        urls.iter()
            .map(|u| regs_get_owned(u.clone(), key.to_string(), "regulations.gov comments")),
    )
    .await;

    let mut total: u64 = 0;
    for (value, warns) in fetched {
        out.warnings.extend(warns);
        let Some(v) = value else { continue };
        total += v
            .get("meta")
            .and_then(|m| m.get("totalElements"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
            for item in arr {
                if let Some(rec) = list_to_record(item, req.docket.as_deref()) {
                    out.comments.push(rec);
                }
            }
        }
    }
    if total > 0 {
        out.total_available = Some(total);
    }

    // Newest first, then trim: each target list was independently sorted.
    out.comments
        .sort_by(|a, b| b.posted_date.cmp(&a.posted_date));
    out.comments.truncate(req.limit);

    if req.with_text {
        hydrate_details(out, key).await;
    } else if !out.comments.is_empty() {
        out.warnings.push(
            "list view only: submitter, organization and comment body live on the detail \
             endpoint. Re-run with --text to fetch them (one request per comment)."
                .to_string(),
        );
    }

    if out.comments.is_empty() && out.warnings.len() <= 1 {
        out.warnings
            .push("regulations.gov returned no comments for this query".to_string());
    }
    if out.total_available.unwrap_or(0) > 5_000 {
        out.warnings.push(
            "regulations.gov caps any one query at 5,000 rows (page[number] max 20). To walk a \
             larger docket, re-run with --after set to the oldest postedDate you already have."
                .to_string(),
        );
    }
}

/// Build a `/comments` list URL. `key_len` is unused except to keep the key out
/// of the returned string — the key goes in the `X-Api-Key` header, never in
/// `source_url`, which we hand back to the caller verbatim.
fn comments_url(
    req: &CommentsRequest,
    comment_on_id: Option<&str>,
    page_size: usize,
    _key_len: usize,
) -> String {
    let mut url = format!("{REGS_BASE}/comments?page%5Bsize%5D={page_size}&sort=-postedDate");
    if let Some(oid) = comment_on_id {
        url.push_str(&format!(
            "&filter%5BcommentOnId%5D={}",
            urlencoding::encode(oid)
        ));
    }
    if let Some(q) = req.query.as_deref() {
        url.push_str(&format!(
            "&filter%5BsearchTerm%5D={}",
            urlencoding::encode(q)
        ));
    }
    if let Some(a) = req.agency.as_deref() {
        url.push_str(&format!("&filter%5BagencyId%5D={}", urlencoding::encode(a)));
    }
    if let Some(d) = req.posted_after.as_deref() {
        url.push_str(&format!(
            "&filter%5BpostedDate%5D%5Bge%5D={}",
            urlencoding::encode(d)
        ));
    }
    url
}

/// List a docket's documents and return `(documentId, objectId)` for the ones
/// worth chasing comments on, most recent first.
async fn docket_comment_targets(
    docket: &str,
    key: &str,
    out: &mut CommentsResponse,
) -> Vec<(String, String)> {
    let url = format!(
        "{REGS_BASE}/documents?filter%5BdocketId%5D={}&page%5Bsize%5D=25&sort=-postedDate",
        urlencoding::encode(docket)
    );
    let Some(v) = regs_get(&url, key, "regulations.gov documents", &mut out.warnings).await else {
        return Vec::new();
    };
    let mut targets: Vec<(String, String)> = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            let doc_id = item.get("id").and_then(|x| x.as_str()).unwrap_or_default();
            let attrs = item.get("attributes");
            let object_id = attrs
                .and_then(|a| a.get("objectId"))
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            if object_id.is_empty() {
                continue;
            }
            let doc_type = attrs
                .and_then(|a| a.get("documentType"))
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            // Supporting material and "Other" attachments carry objectIds too
            // but almost never carry comments; rules and notices do.
            if matches!(doc_type, "Rule" | "Proposed Rule" | "Notice") {
                targets.push((doc_id.to_string(), object_id.to_string()));
            }
        }
    }
    targets
}

/// `--text`: pull each comment's detail record for the body, submitter and
/// attachment URLs. Bounded concurrency — regulations.gov meters per hour and
/// a 250-comment page would otherwise burn a quarter of a free key's budget in
/// one burst.
async fn hydrate_details(out: &mut CommentsResponse, key: &str) {
    const CHUNK: usize = 5;
    let ids: Vec<String> = out
        .comments
        .iter()
        .filter_map(|c| c.id.clone())
        .filter(|_| true)
        .collect();

    let mut details: Vec<(String, serde_json::Value)> = Vec::new();
    for chunk in ids.chunks(CHUNK) {
        let results = futures::future::join_all(chunk.iter().map(|id| {
            let url = format!("{REGS_BASE}/comments/{id}?include=attachments");
            let key = key.to_string();
            let id = id.clone();
            async move {
                let (v, w) = regs_get_owned(url, key, "regulations.gov comment detail").await;
                (id, v, w)
            }
        }))
        .await;
        for (id, value, warns) in results {
            out.warnings.extend(warns);
            if let Some(v) = value {
                details.push((id, v));
            }
        }
    }

    for (id, v) in details {
        let Some(full) = detail_to_record(&v) else {
            continue;
        };
        if let Some(slot) = out.comments.iter_mut().find(|c| c.id.as_deref() == Some(&id)) {
            // Detail wins where it has a value; the list row keeps whatever
            // detail omitted (the two field sets are not a superset relation).
            slot.comment_text = full.comment_text.or_else(|| slot.comment_text.clone());
            slot.submitter = full.submitter.or_else(|| slot.submitter.clone());
            slot.organization = full.organization.or_else(|| slot.organization.clone());
            slot.tracking_number = full.tracking_number.or_else(|| slot.tracking_number.clone());
            slot.docket_id = full.docket_id.or_else(|| slot.docket_id.clone());
            slot.attachment_urls = full.attachment_urls;
        }
    }

    let with_attachments = out
        .comments
        .iter()
        .filter(|c| !c.attachment_urls.is_empty())
        .count();
    if with_attachments > 0 {
        out.warnings.push(format!(
            "{with_attachments} of {} comments carry attachments — for institutional filers the \
             argument is in the attached PDF, and `comment_text` says only \"See attached.\"",
            out.comments.len()
        ));
    }
}

/// Map a `/comments` list row. The list only carries 8 attributes; submitter,
/// organization and body require the detail endpoint.
fn list_to_record(item: &serde_json::Value, docket: Option<&str>) -> Option<CommentRecord> {
    let id = item.get("id").and_then(|x| x.as_str())?.to_string();
    let attrs = item.get("attributes");
    let s = |k: &str| {
        attrs
            .and_then(|a| a.get(k))
            .and_then(|x| x.as_str())
            .map(str::to_string)
    };
    Some(CommentRecord {
        url: Some(format!("https://www.regulations.gov/comment/{id}")),
        id: Some(id),
        tracking_number: None,
        title: s("title"),
        submitter: None,
        organization: None,
        posted_date: s("postedDate").map(|d| iso_day(&d)),
        docket_id: docket.map(str::to_string),
        agency_id: s("agencyId"),
        comment_text: s("highlightedContent").map(|h| strip_markup(&h)),
        attachment_urls: Vec::new(),
        record_type: "comment".to_string(),
    })
}

/// Map a `/comments/{id}?include=attachments` detail document.
fn detail_to_record(v: &serde_json::Value) -> Option<CommentRecord> {
    let data = v.get("data")?;
    let id = data.get("id").and_then(|x| x.as_str())?.to_string();
    let attrs = data.get("attributes");
    let s = |k: &str| {
        attrs
            .and_then(|a| a.get(k))
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .filter(|x| !x.trim().is_empty())
    };

    // regulations.gov splits the human name across two fields and leaves both
    // empty for anonymous filers, which is most of a mass-comment campaign.
    let submitter = match (s("firstName"), s("lastName")) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    };

    let body = s("comment").map(|b| {
        let plain = strip_markup(&b);
        clamp_text(&plain, COMMENT_TEXT_CAP).0
    });

    Some(CommentRecord {
        url: Some(format!("https://www.regulations.gov/comment/{id}")),
        id: Some(id),
        tracking_number: s("trackingNbr"),
        title: s("title"),
        submitter,
        organization: s("organization"),
        posted_date: s("postedDate").map(|d| iso_day(&d)),
        docket_id: s("docketId"),
        agency_id: s("agencyId"),
        comment_text: body,
        attachment_urls: attachment_urls(v),
        record_type: "comment".to_string(),
    })
}

/// `include=attachments` puts the files in a sibling `included[]` array, one
/// resource per attachment with a `fileFormats[]` of `{fileUrl, format, size}`.
fn attachment_urls(v: &serde_json::Value) -> Vec<String> {
    let mut urls = Vec::new();
    let Some(included) = v.get("included").and_then(|i| i.as_array()) else {
        return urls;
    };
    for res in included {
        if res.get("type").and_then(|t| t.as_str()) != Some("attachments") {
            continue;
        }
        let formats = res
            .get("attributes")
            .and_then(|a| a.get("fileFormats"))
            .and_then(|f| f.as_array());
        for f in formats.into_iter().flatten() {
            if let Some(u) = f.get("fileUrl").and_then(|x| x.as_str()) {
                urls.push(u.to_string());
            }
        }
    }
    urls
}

// ── key-free path: the Federal Register rulemaking record ──────────────────

async fn without_key(req: &CommentsRequest, out: &mut CommentsResponse) {
    if let Some(id) = req.comment_id.as_deref() {
        out.warnings.push(format!(
            "comment {id} can only be fetched with a key; the public page \
             https://www.regulations.gov/comment/{id} is rendered client-side from the same \
             key-gated API, so it is not scrapable either"
        ));
        return;
    }

    if let Some(docket) = req.docket.as_deref() {
        fr_docket_record(docket, req, out).await;
        return;
    }

    if let Some(q) = req.query.as_deref() {
        fr_term_record(q, req, out).await;
        return;
    }

    out.warnings
        .push("nothing to look up: pass --docket, --q or --id".to_string());
}

/// Find the Federal Register documents that belong to a regulations.gov docket.
///
/// There is no direct lookup: FR's own `docket_id` condition indexes the
/// *agency's* docket number ("S7-06-22"), not the regulations.gov id
/// ("SEC-2026-5190") — querying it for the latter returns count 0, verified.
/// The only link is `regulations_dot_gov_info.document_id`, which is
/// `{docket}-{seq}`, so we try the cheap direct condition first and otherwise
/// scan that agency's documents for the year encoded in the docket id and match
/// the prefix client-side.
async fn fr_docket_record(docket: &str, req: &CommentsRequest, out: &mut CommentsResponse) {
    // Cheap first: the caller may have handed us an agency docket number.
    let direct = format!(
        "{FR_BASE}/documents.json?conditions%5Bdocket_id%5D={}&per_page=20&order=newest&{}",
        urlencoding::encode(docket),
        fr_fields()
    );
    if let Some(v) = fr_get(&direct, "federal register docket", &mut out.warnings).await {
        let n = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        if n > 0 {
            out.source_url = Some(direct);
            absorb_fr_results(&v, req, out);
            note_docket_links(docket, out);
            return;
        }
    }

    // Scan path. Narrow by agency and by the year in the docket id — without
    // both, this is a walk over tens of thousands of documents.
    let Some(acronym) = docket_agency(docket) else {
        out.warnings.push(format!(
            "cannot map {docket:?} to a Federal Register agency (expected AGENCY-YYYY-NNNN); \
             no key-free fallback available for it"
        ));
        return;
    };
    let Some(slug) = agency_slug(&acronym, &mut out.warnings).await else {
        out.warnings.push(format!(
            "no Federal Register agency matches {acronym:?}; the docket may belong to an agency \
             that does not publish in the FR"
        ));
        return;
    };

    let since = req
        .posted_after
        .clone()
        .or_else(|| docket_year(docket).map(|y| format!("{y}-01-01")))
        .unwrap_or_else(|| format!("{}-01-01", Utc::now().date_naive().format("%Y")));

    // Two-phase so we don't pull ~700 KB of prose to test a string prefix:
    // scan with two fields, then fetch full records for the handful that match.
    let scan = format!(
        "{FR_BASE}/documents.json?conditions%5Bagencies%5D%5B%5D={slug}\
         &conditions%5Bpublication_date%5D%5Bgte%5D={since}&per_page=1000&order=newest\
         &fields%5B%5D=document_number&fields%5B%5D=regulations_dot_gov_info"
    );
    let Some(v) = fr_get(&scan, "federal register scan", &mut out.warnings).await else {
        return;
    };

    let prefix = format!("{}-", docket.to_uppercase());
    let mut matched: Vec<String> = Vec::new();
    if let Some(arr) = v.get("results").and_then(|r| r.as_array()) {
        for doc in arr {
            let doc_id = doc
                .get("regulations_dot_gov_info")
                .and_then(|i| i.get("document_id"))
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            if doc_id.to_uppercase().starts_with(&prefix) {
                if let Some(n) = doc.get("document_number").and_then(|x| x.as_str()) {
                    matched.push(n.to_string());
                }
            }
        }
    }

    let total = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
    if matched.is_empty() {
        out.source_url = Some(scan);
        out.warnings.push(format!(
            "scanned {} {acronym} Federal Register documents published since {since} and none \
             carried docket {docket}. The docket may predate {since}, or it may be a notice-only \
             docket that never reached the Federal Register.",
            total.min(1000)
        ));
        note_docket_links(docket, out);
        return;
    }
    if total > 1000 {
        out.warnings.push(format!(
            "Federal Register scan is capped at 1,000 documents per page and {acronym} published \
             {total} since {since}; a docket older than that window can be missed. Narrow with \
             --after."
        ));
    }

    let detail = format!(
        "{FR_BASE}/documents/{}.json?{}",
        matched.join(","),
        fr_fields()
    );
    out.source_url = Some(detail.clone());
    if let Some(dv) = fr_get(&detail, "federal register documents", &mut out.warnings).await {
        // A single id returns a bare object, a list returns {count,results}.
        if dv.get("results").is_some() {
            absorb_fr_results(&dv, req, out);
        } else {
            if let Some(rec) = fr_to_record(&dv) {
                out.total_available = fr_comment_count(&dv).or(out.total_available);
                out.comments.push(rec);
            }
        }
    }
    note_docket_links(docket, out);
}

/// Key-free `--q`: search the Federal Register for rules matching the term and
/// hand back the ones that have a regulations.gov docket, so the caller gets
/// docket ids to feed back in once they have a key.
async fn fr_term_record(query: &str, req: &CommentsRequest, out: &mut CommentsResponse) {
    let mut url = format!(
        "{FR_BASE}/documents.json?conditions%5Bterm%5D={}&per_page={}&order=newest&{}",
        urlencoding::encode(query),
        req.limit.clamp(5, 100),
        fr_fields()
    );
    if let Some(a) = req.agency.as_deref() {
        if let Some(slug) = agency_slug(&a.to_uppercase(), &mut out.warnings).await {
            url.push_str(&format!("&conditions%5Bagencies%5D%5B%5D={slug}"));
        }
    }
    if let Some(d) = req.posted_after.as_deref() {
        url.push_str(&format!(
            "&conditions%5Bpublication_date%5D%5Bgte%5D={d}"
        ));
    }
    out.source_url = Some(url.clone());
    let Some(v) = fr_get(&url, "federal register search", &mut out.warnings).await else {
        return;
    };
    absorb_fr_results(&v, req, out);
    out.warnings.push(
        "these are Federal Register rulemaking documents matching the term, not comments. Each \
         row's `docket_id` (where present) is the regulations.gov docket to pass to --docket once \
         a key is set."
            .to_string(),
    );
}

fn absorb_fr_results(v: &serde_json::Value, req: &CommentsRequest, out: &mut CommentsResponse) {
    let mut counted: u64 = 0;
    if let Some(arr) = v.get("results").and_then(|r| r.as_array()) {
        for doc in arr.iter().take(req.limit) {
            counted += fr_comment_count(doc).unwrap_or(0);
            if let Some(rec) = fr_to_record(doc) {
                out.comments.push(rec);
            }
        }
    }
    if counted > 0 {
        out.total_available = Some(counted);
    }
}

fn fr_comment_count(doc: &serde_json::Value) -> Option<u64> {
    doc.get("regulations_dot_gov_info")?
        .get("comments_count")?
        .as_u64()
}

/// Represent a Federal Register rulemaking document as a `CommentRecord` so it
/// travels in the same list — tagged `rulemaking_document`, never `comment`.
fn fr_to_record(doc: &serde_json::Value) -> Option<CommentRecord> {
    let number = doc.get("document_number").and_then(|x| x.as_str())?;
    let s = |k: &str| doc.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let info = doc.get("regulations_dot_gov_info");
    let regs_doc_id = info
        .and_then(|i| i.get("document_id"))
        .and_then(|x| x.as_str());
    let agency_id = info
        .and_then(|i| i.get("agency_id"))
        .and_then(|x| x.as_str())
        .map(str::to_string);

    // "SEC-2026-5190-0001" -> "SEC-2026-5190".
    let docket_id = regs_doc_id.map(|d| {
        let parts: Vec<&str> = d.rsplitn(2, '-').collect();
        if parts.len() == 2 {
            parts[1].to_string()
        } else {
            d.to_string()
        }
    });

    let deadline = s("comments_close_on");
    let mut summary = s("abstract").map(|a| strip_markup(&a)).unwrap_or_default();
    if let Some(d) = deadline.as_deref() {
        summary = format!("[comments close {d}] {summary}");
    }
    let count = fr_comment_count(doc);
    if let Some(c) = count {
        summary = format!("{summary} [{c} comments filed as of the FR's last check]");
    }

    Some(CommentRecord {
        id: Some(number.to_string()),
        tracking_number: None,
        title: s("title"),
        submitter: None,
        organization: None,
        posted_date: s("publication_date"),
        docket_id,
        agency_id,
        comment_text: Some(clamp_text(summary.trim(), COMMENT_TEXT_CAP).0),
        attachment_urls: Vec::new(),
        url: s("html_url"),
        record_type: "rulemaking_document".to_string(),
    })
}

fn note_docket_links(docket: &str, out: &mut CommentsResponse) {
    out.warnings.push(format!(
        "docket {docket} browses at https://www.regulations.gov/docket/{docket}/comments — the \
         page loads its comments from the same key-gated API, so a key is the only programmatic \
         route to their text"
    ));
}

const FR_FALLBACK_FIELDS: &[&str] = &[
    "document_number",
    "title",
    "abstract",
    "type",
    "publication_date",
    "comments_close_on",
    "comment_url",
    "docket_ids",
    "regulations_dot_gov_info",
    "html_url",
];

fn fr_fields() -> String {
    FR_FALLBACK_FIELDS
        .iter()
        .map(|f| format!("fields%5B%5D={f}"))
        .collect::<Vec<_>>()
        .join("&")
}

// ── helpers ────────────────────────────────────────────────────────────────

async fn regs_get(
    url: &str,
    key: &str,
    source: &str,
    warnings: &mut Vec<String>,
) -> Option<serde_json::Value> {
    let (v, w) = regs_get_owned(url.to_string(), key.to_string(), source).await;
    warnings.extend(w);
    v
}

/// Owned-argument twin of `regs_get` so several of these can be `join_all`ed;
/// warnings come back in the tuple because they can't share a `&mut Vec`.
async fn regs_get_owned(
    url: String,
    key: String,
    source: &str,
) -> (Option<serde_json::Value>, Vec<String>) {
    let mut warnings = Vec::new();
    // Header, not query string: keeps the key out of `source_url` and out of
    // any upstream access log we don't control.
    let resp = match shared_client::GENERAL
        .get(&url)
        .header("X-Api-Key", &key)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("{source} request failed: {e}"));
            return (None, warnings);
        }
    };
    // api.data.gov answers 403 for both a missing and an invalid key (govinfo
    // uses 401 for the same condition), so spell out what the caller should
    // check rather than leaving them with a bare status code.
    let status = resp.status();
    let Some(resp) = soft_fail(source, resp, &mut warnings).await else {
        if status.as_u16() == 403 {
            warnings.push(
                "403 from api.data.gov usually means the key is wrong, not missing — check \
                 REGULATIONS_GOV_API_KEY for stray whitespace, and that it is an api.data.gov key \
                 rather than a regulations.gov account password"
                    .to_string(),
            );
        }
        return (None, warnings);
    };
    match resp.text().await {
        Ok(body) => match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => {
                // Validation errors arrive as 200-with-errors on some paths.
                if let Some(errs) = v.get("errors").and_then(|e| e.as_array()) {
                    for e in errs {
                        let t = e.get("title").and_then(|x| x.as_str()).unwrap_or("error");
                        warnings.push(format!("{source}: {t}"));
                    }
                    return (None, warnings);
                }
                (Some(v), warnings)
            }
            Err(e) => {
                warnings.push(format!("{source} parse failed: {e}"));
                (None, warnings)
            }
        },
        Err(e) => {
            warnings.push(format!("{source} body read failed: {e}"));
            (None, warnings)
        }
    }
}

async fn fr_get(
    url: &str,
    source: &str,
    warnings: &mut Vec<String>,
) -> Option<serde_json::Value> {
    let resp = match shared_client::GENERAL.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("{source} request failed: {e}"));
            return None;
        }
    };
    let resp = soft_fail(source, resp, warnings).await?;
    match resp.text().await {
        Ok(b) => match serde_json::from_str::<serde_json::Value>(&b) {
            Ok(v) => Some(v),
            Err(e) => {
                warnings.push(format!("{source} parse failed: {e}"));
                None
            }
        },
        Err(e) => {
            warnings.push(format!("{source} body read failed: {e}"));
            None
        }
    }
}

/// "SEC-2026-5190" -> "SEC". Regulations.gov docket ids are
/// `{AGENCY}-{YYYY}-{NNNN}`, and a few legacy ones are `{AGENCY}-{YYYY}-N-...`.
fn docket_agency(docket: &str) -> Option<String> {
    let head = docket.split('-').next()?.trim();
    if head.is_empty() || !head.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(head.to_uppercase())
}

/// "SEC-2026-5190" -> 2026.
fn docket_year(docket: &str) -> Option<i32> {
    let seg = docket.split('-').nth(1)?;
    let y: i32 = seg.parse().ok()?;
    (1990..=2100).contains(&y).then_some(y)
}

/// Common regulations.gov agency acronyms to Federal Register agency slugs.
/// Baked in because the authoritative mapping is a 690 KB `/agencies.json`
/// that we only want to pay for on a miss.
const AGENCY_SLUGS: &[(&str, &str)] = &[
    ("SEC", "securities-and-exchange-commission"),
    ("CFTC", "commodity-futures-trading-commission"),
    ("CFPB", "consumer-financial-protection-bureau"),
    ("FINCEN", "financial-crimes-enforcement-network"),
    ("OCC", "comptroller-of-the-currency"),
    ("FDIC", "federal-deposit-insurance-corporation"),
    ("FRS", "federal-reserve-system"),
    ("TREAS", "treasury-department"),
    ("IRS", "internal-revenue-service"),
    ("FHFA", "federal-housing-finance-agency"),
    ("EPA", "environmental-protection-agency"),
    ("FTC", "federal-trade-commission"),
    ("FCC", "federal-communications-commission"),
    ("DOL", "labor-department"),
    ("OSHA", "occupational-safety-and-health-administration"),
    ("FDA", "food-and-drug-administration"),
    ("HHS", "health-and-human-services-department"),
    ("DOT", "transportation-department"),
    ("FAA", "federal-aviation-administration"),
    ("NHTSA", "national-highway-traffic-safety-administration"),
    ("DHS", "homeland-security-department"),
    ("ED", "education-department"),
    ("NRC", "nuclear-regulatory-commission"),
    ("BIS", "industry-and-security-bureau"),
    ("USCG", "coast-guard"),
    ("FEMA", "federal-emergency-management-agency"),
];

/// Cached once per process — `/agencies.json` is 690 KB and changes a few times
/// a year.
static FR_AGENCIES: tokio::sync::OnceCell<Vec<(String, String)>> =
    tokio::sync::OnceCell::const_new();

async fn agency_slug(acronym: &str, warnings: &mut Vec<String>) -> Option<String> {
    let upper = acronym.to_uppercase();
    if let Some((_, slug)) = AGENCY_SLUGS.iter().find(|(a, _)| *a == upper) {
        return Some((*slug).to_string());
    }
    let table = FR_AGENCIES
        .get_or_init(|| async {
            let mut w = Vec::new();
            let url = format!("{FR_BASE}/agencies.json");
            match fr_get(&url, "federal register agencies", &mut w).await {
                Some(v) => v
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|a| {
                                let short = a.get("short_name").and_then(|x| x.as_str())?;
                                let slug = a.get("slug").and_then(|x| x.as_str())?;
                                Some((short.to_uppercase(), slug.to_string()))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                None => Vec::new(),
            }
        })
        .await;
    if table.is_empty() {
        warnings.push("could not load the Federal Register agency list".to_string());
    }
    table
        .iter()
        .find(|(a, _)| *a == upper)
        .map(|(_, s)| s.clone())
}

/// regulations.gov posts dates as `2020-08-10T11:58:52Z`; callers want the day.
fn iso_day(ts: &str) -> String {
    ts.split('T').next().unwrap_or(ts).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docket_id_is_split_into_agency_and_year() {
        assert_eq!(docket_agency("SEC-2026-5190").as_deref(), Some("SEC"));
        assert_eq!(docket_agency("finCEN-2024-0006").as_deref(), Some("FINCEN"));
        assert_eq!(docket_year("SEC-2026-5190"), Some(2026));
        assert_eq!(docket_year("SEC"), None);
        // An FR-style agency docket has no year segment; the scan path must
        // not invent one.
        assert_eq!(docket_year("S7-06-22"), None);
    }

    #[test]
    fn known_agencies_resolve_without_a_network_call() {
        // agency_slug() is async only for the miss path; the hit path is the
        // table, so assert the table directly.
        assert_eq!(
            AGENCY_SLUGS.iter().find(|(a, _)| *a == "SEC").map(|(_, s)| *s),
            Some("securities-and-exchange-commission")
        );
        assert!(AGENCY_SLUGS.iter().all(|(a, _)| a
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())));
    }

    #[test]
    fn fr_document_maps_to_a_tagged_rulemaking_record() {
        let doc: serde_json::Value = serde_json::from_str(
            r#"{"document_number":"2026-17183","title":"Regulation Crypto Assets",
                "abstract":"The Commission is proposing <em>rules</em>.",
                "publication_date":"2026-08-21","comments_close_on":"2026-10-20",
                "html_url":"https://www.federalregister.gov/documents/2026/08/21/2026-17183/x",
                "regulations_dot_gov_info":{"comments_count":42,"agency_id":"SEC",
                  "document_id":"SEC-2026-5190-0001"}}"#,
        )
        .expect("fixture parses");
        let rec = fr_to_record(&doc).expect("maps");
        assert_eq!(rec.record_type, "rulemaking_document");
        assert_eq!(rec.docket_id.as_deref(), Some("SEC-2026-5190"));
        assert_eq!(rec.agency_id.as_deref(), Some("SEC"));
        assert_eq!(rec.posted_date.as_deref(), Some("2026-08-21"));
        let text = rec.comment_text.expect("summary");
        assert!(text.starts_with("[comments close 2026-10-20]"), "{text}");
        assert!(text.contains("42 comments filed"), "{text}");
        // Markup stripped, not passed through.
        assert!(!text.contains("<em>"), "{text}");
        assert_eq!(fr_comment_count(&doc), Some(42));
    }

    #[test]
    fn comment_detail_merges_names_and_collects_attachments() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"data":{"id":"SEC-2026-5190-0031","type":"comments",
                 "attributes":{"title":"Comment from Big Bank","firstName":"Jane",
                   "lastName":"Roe","organization":"Big Bank, N.A.",
                   "comment":"See attached.","trackingNbr":"abc-123",
                   "docketId":"SEC-2026-5190","agencyId":"SEC",
                   "postedDate":"2026-09-04T04:00:00Z"}},
                "included":[{"type":"attachments","attributes":{
                   "fileFormats":[{"fileUrl":"https://downloads.regulations.gov/a.pdf",
                                   "format":"pdf","size":90210}]}},
                  {"type":"other","attributes":{}}]}"#,
        )
        .expect("fixture parses");
        let rec = detail_to_record(&v).expect("maps");
        assert_eq!(rec.submitter.as_deref(), Some("Jane Roe"));
        assert_eq!(rec.organization.as_deref(), Some("Big Bank, N.A."));
        assert_eq!(rec.posted_date.as_deref(), Some("2026-09-04"));
        assert_eq!(rec.record_type, "comment");
        assert_eq!(
            rec.attachment_urls,
            vec!["https://downloads.regulations.gov/a.pdf".to_string()]
        );
        assert_eq!(
            rec.url.as_deref(),
            Some("https://www.regulations.gov/comment/SEC-2026-5190-0031")
        );
    }

    #[test]
    fn anonymous_comments_leave_the_submitter_empty_rather_than_guessing() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"data":{"id":"X-1","attributes":{"firstName":"","lastName":"",
                 "comment":"Please do not adopt this rule."}}}"#,
        )
        .expect("fixture parses");
        let rec = detail_to_record(&v).expect("maps");
        assert!(rec.submitter.is_none());
        assert!(rec.organization.is_none());
        assert!(rec.attachment_urls.is_empty());
    }

    #[test]
    fn page_size_respects_the_upstream_minimum_of_five() {
        assert_eq!(3usize.clamp(REGS_PAGE_MIN, REGS_PAGE_MAX), 5);
        assert_eq!(25usize.clamp(REGS_PAGE_MIN, REGS_PAGE_MAX), 25);
        assert_eq!(9_000usize.clamp(REGS_PAGE_MIN, REGS_PAGE_MAX), 250);
    }

    #[test]
    fn comments_url_carries_the_filters_and_never_the_key() {
        let req = CommentsRequest {
            docket: Some("SEC-2026-5190".into()),
            query: Some("crypto custody".into()),
            agency: Some("SEC".into()),
            posted_after: Some("2026-01-01".into()),
            comment_id: None,
            with_text: false,
            limit: 25,
        };
        let url = comments_url(&req, Some("0900006483a6cba3"), 25, 40);
        assert!(url.contains("filter%5BcommentOnId%5D=0900006483a6cba3"), "{url}");
        assert!(url.contains("filter%5BsearchTerm%5D=crypto%20custody"), "{url}");
        assert!(url.contains("filter%5BpostedDate%5D%5Bge%5D=2026-01-01"), "{url}");
        assert!(url.contains("sort=-postedDate"), "{url}");
        assert!(!url.contains("api_key"), "{url}");
    }

    #[test]
    fn timestamps_are_reduced_to_the_day() {
        assert_eq!(iso_day("2026-09-04T04:00:00Z"), "2026-09-04");
        assert_eq!(iso_day("2026-09-04"), "2026-09-04");
    }

    #[test]
    fn no_key_message_names_the_signup_and_the_env_var() {
        let m = no_key_message();
        assert!(m.contains("https://api.data.gov/signup/"));
        assert!(m.contains("REGULATIONS_GOV_API_KEY"));
    }
}
