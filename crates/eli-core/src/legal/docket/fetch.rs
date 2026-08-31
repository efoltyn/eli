//! Federal and Supreme Court docket sheets — the row-by-row procedural history
//! of a case.
//!
//! Why this exists when a search engine already indexes the whole web: docket
//! sheets are not on the open web. PACER is the primary source and it is
//! paywalled per page; the commercial mirrors (Docket Alarm, Bloomberg Law,
//! PacerMonitor) are paywalled too, so a query like "docket in United States v.
//! Bankman-Fried, No. 1:22-cr-00673 (S.D.N.Y.)" returns a wall of subscription
//! landing pages and news articles *about* the case, never the entries. This
//! module returns the entries themselves — number, filing date, the clerk's
//! description, and a direct, auth-free PDF link for every filing someone has
//! already liberated into the RECAP Archive.
//!
//! Three upstreams, in descending order of preference and ascending order of
//! cost:
//!
//!   * `supremecourt.gov/rss/cases/JSON/{docket}.json` — undocumented but
//!     stable, key-free, unthrottled, and *complete*: every proceeding line and
//!     every brief PDF on a SCOTUS docket. Strictly better than anything
//!     CourtListener has for `scotus`, so it is tried first and never costs
//!     quota.
//!   * CourtListener `/dockets/` + `/docket-entries/` — the authoritative
//!     docket sheet, but 401 without a token.
//!   * CourtListener `/search/?type=r` and `?type=rd` — anonymously readable
//!     (measured), and `type=rd` with a fielded `q=docket_id:<id>` returns real
//!     per-filing rows. This is the key-free fallback that keeps the tool
//!     answering with no credentials at all.
//!
//! Rate limiting is the design constraint everywhere below CourtListener:
//! anonymous callers get 100/day *and* 5/min/50/hour/125/day keyed on client
//! IP, so every path here is strictly sequential, spaced, and page-capped.

use crate::legal::{courtlistener as cl, shared_client, soft_fail};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// PDFs and MP3s live here. No auth, and explicitly outside the DRF throttle —
/// so a `filepath_local` turned absolute is a link the caller can just fetch.
const CL_STORAGE: &str = "https://storage.courtlistener.com";
const SCOTUS_JSON: &str = "https://www.supremecourt.gov/rss/cases/JSON";

/// The search endpoint pages 20 at a time and ignores `page_size`.
const SEARCH_PAGE: usize = 20;
/// Hard ceiling on paged calls. Six pages of filings is 120 rows; going deeper
/// would spend a meaningful slice of a 100/day anonymous budget on one call.
const MAX_SEARCH_PAGES: usize = 6;
/// The token path pages 100 at a time, so eight pages covers an 800-entry
/// docket — past that the 5/min throttle makes it hopeless anyway.
const MAX_ENTRY_PAGES: usize = 8;
/// Deliberate spacing between paged calls. Not a substitute for the throttle,
/// just an assurance we never burst.
const PAGE_DELAY: Duration = Duration::from_millis(400);

#[derive(Clone, Debug)]
pub struct DocketRequest {
    pub court: Option<String>,
    pub docket_number: Option<String>,
    pub docket_id: Option<u64>,
    pub query: Option<String>,
    pub include_entries: bool,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RecapDocument {
    pub document_number: Option<String>,
    pub attachment_number: Option<u32>,
    pub description: Option<String>,
    pub page_count: Option<u32>,
    /// Whether the binary actually exists in the archive. False means the row
    /// is metadata only — nobody has bought this filing out of PACER yet.
    pub is_available: bool,
    /// Absolute URL by the time it reaches the caller, not the raw relative
    /// path the API returns.
    pub filepath_local: Option<String>,
    pub pacer_url: Option<String>,
    pub plain_text_chars: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DocketEntry {
    pub entry_number: Option<u64>,
    pub date_filed: Option<String>,
    pub description: Option<String>,
    pub documents: Vec<RecapDocument>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocketResponse {
    pub generated_at: DateTime<Utc>,
    pub docket_id: Option<u64>,
    pub case_name: Option<String>,
    pub court: Option<String>,
    pub docket_number: Option<String>,
    pub date_filed: Option<String>,
    pub date_terminated: Option<String>,
    pub nature_of_suit: Option<String>,
    pub cause: Option<String>,
    pub assigned_to: Option<String>,
    pub jury_demand: Option<String>,
    /// Total entries upstream reports, NOT `entries.len()` — a caller paging a
    /// 3,000-row docket has to know what it is paging through.
    pub entry_count: usize,
    pub entries: Vec<DocketEntry>,
    pub candidates: Vec<Value>,
    pub url: Option<String>,
    /// Which upstream actually answered: "supremecourt.gov", "courtlistener
    /// (token)", or "courtlistener recap search (key-free)". Provenance matters
    /// here because the three differ sharply in completeness.
    pub source: Option<String>,
    pub warnings: Vec<String>,
}

impl DocketResponse {
    fn empty() -> Self {
        Self {
            generated_at: Utc::now(),
            docket_id: None,
            case_name: None,
            court: None,
            docket_number: None,
            date_filed: None,
            date_terminated: None,
            nature_of_suit: None,
            cause: None,
            assigned_to: None,
            jury_demand: None,
            entry_count: 0,
            entries: Vec::new(),
            candidates: Vec::new(),
            url: None,
            source: None,
            warnings: Vec::new(),
        }
    }
}

pub async fn fetch_docket(req: DocketRequest) -> Result<DocketResponse> {
    let mut out = DocketResponse::empty();

    // SCOTUS first and unconditionally: supremecourt.gov is key-free and
    // unthrottled, so trying it costs nothing even when it misses, and it
    // beats every CourtListener path for this one court.
    if let Some(key) = scotus_target(req.court.as_deref(), req.docket_number.as_deref()) {
        scotus_docket(&key, &req, &mut out).await;
        if out.case_name.is_some() {
            return Ok(out);
        }
        out.warnings.push(format!(
            "supremecourt.gov has no docket {key}. Supreme Court cases are not in RECAP, so there \
             is no docket-sheet fallback; try `legal search --kind o --court scotus` for the \
             opinions instead."
        ));
        out.court = Some("scotus".to_string());
        out.docket_number = req.docket_number.clone();
        return Ok(out);
    }

    if let Some(id) = req.docket_id {
        by_docket_id(id, &req, &mut out).await;
        return Ok(out);
    }
    if let (Some(court), Some(number)) = (req.court.as_deref(), req.docket_number.as_deref()) {
        by_court_and_number(court, number, &req, &mut out).await;
        return Ok(out);
    }
    if let Some(number) = req.docket_number.as_deref() {
        // A bare docket number is ambiguous across ~3,300 courts; search can
        // still fuzzy-match it, but say so rather than pretending otherwise.
        out.warnings.push(
            "no --court given: docket numbers repeat across courts, so these are candidates, not \
             a match. Re-run with --court to pin one."
                .to_string(),
        );
        by_search(&format!("docketNumber:\"{number}\""), None, &req, &mut out).await;
        return Ok(out);
    }
    if let Some(q) = req.query.as_deref() {
        by_search(q, None, &req, &mut out).await;
        return Ok(out);
    }

    Err(Error::InvalidInput(
        "docket requires --id, or --number (ideally with --court), or --q".into(),
    ))
}

// ── SCOTUS: supremecourt.gov, key-free and complete ────────────────────────

/// Decide whether this request should go to supremecourt.gov, and with what key.
///
/// An explicit `--court scotus` always routes here. Without a court we sniff the
/// docket number, because a bare "22-451" is far more likely to be a Supreme
/// Court docket than anything else a user types — but only when no court was
/// named, since "23-3018" is also a perfectly good Ninth Circuit number.
fn scotus_target(court: Option<&str>, number: Option<&str>) -> Option<String> {
    let number = number?.trim();
    if number.is_empty() {
        return None;
    }
    let is_scotus_court = court
        .map(|c| matches!(c.trim().to_ascii_lowercase().as_str(), "scotus" | "us" | "supreme"))
        .unwrap_or(false);
    if is_scotus_court || (court.is_none() && is_scotus_docket_number(number)) {
        return Some(scotus_key(number));
    }
    None
}

/// Supreme Court docket numbers are `<2-digit term><separator><sequence>`:
/// `22-451` (paid/IFP), `23A994` (application to a Circuit Justice),
/// `22O145` (original jurisdiction), `24M12` (motion). Nothing else has that
/// shape — district numbers carry an office prefix and a case-type token
/// ("1:22-cr-00673"), and circuit numbers use four-digit sequences that we
/// deliberately do NOT try to distinguish (hence the court-absent guard above).
fn is_scotus_docket_number(raw: &str) -> bool {
    let s: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    let mut chars = s.chars();
    let (Some(a), Some(b)) = (chars.next(), chars.next()) else {
        return false;
    };
    if !a.is_ascii_digit() || !b.is_ascii_digit() {
        return false;
    }
    let Some(sep) = chars.next() else {
        return false;
    };
    if !matches!(sep, '-' | 'A' | 'M' | 'O') {
        return false;
    }
    let tail: String = chars.collect();
    !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())
}

/// The JSON filename is the docket number with whitespace removed and letters
/// upper-cased: "23a994" -> "23A994".
fn scotus_key(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase()
}

async fn scotus_docket(key: &str, req: &DocketRequest, out: &mut DocketResponse) {
    let url = format!("{SCOTUS_JSON}/{key}.json");
    let Some(v) = get_json(&url, "supremecourt.gov docket", &mut out.warnings).await else {
        return;
    };

    out.source = Some("supremecourt.gov".to_string());
    out.court = Some("scotus".to_string());
    out.docket_number = str_of(&v, "CaseNumber")
        .map(|s| s.trim().to_string())
        .or_else(|| Some(key.to_string()));
    out.case_name = scotus_case_name(&v);
    out.date_filed = str_of(&v, "DocketedDate").and_then(|d| iso_date(&d));
    // SCOTUS has no "nature of suit"; the case-type code (Paid / IFP /
    // Original) is the closest structural analogue and is what practitioners
    // actually key off.
    out.nature_of_suit = str_of(&v, "sJsonCaseType").map(|t| format!("{t} case"));
    out.cause = scotus_lower_court(&v);
    // The docket page itself, which is what a user wants to open.
    out.url = Some(format!(
        "https://www.supremecourt.gov/search.aspx?filename=/docket/docketfiles/html/public/{key}.html"
    ));

    let proceedings = v
        .get("ProceedingsandOrder")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    out.entry_count = proceedings.len();

    if !req.include_entries {
        return;
    }
    // Last proceeding line is the closest thing to a termination date; only
    // trust it once the case is plainly over.
    out.date_terminated = scotus_terminated(&proceedings);

    let (start, end) = page_window(proceedings.len(), req.offset, req.limit);
    for (idx, p) in proceedings[start..end].iter().enumerate() {
        let documents = p
            .get("Links")
            .and_then(|l| l.as_array())
            .map(|links| {
                links
                    .iter()
                    .map(|l| RecapDocument {
                        document_number: None,
                        attachment_number: None,
                        description: str_of(l, "Description").or_else(|| str_of(l, "File")),
                        page_count: None,
                        // supremecourt.gov only lists a link when the PDF is
                        // actually served, so presence is availability.
                        is_available: str_of(l, "DocumentUrl").is_some(),
                        filepath_local: str_of(l, "DocumentUrl"),
                        pacer_url: None,
                        plain_text_chars: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.entries.push(DocketEntry {
            // The feed carries no numbering, so the 1-based position on the
            // docket sheet is the only stable identifier.
            entry_number: Some((start + idx + 1) as u64),
            date_filed: str_of(p, "Date").and_then(|d| iso_date(&d)),
            description: str_of(p, "Text"),
            documents,
        });
    }
    if end < proceedings.len() || start > 0 {
        out.warnings.push(format!(
            "showing docket entries {}-{} of {}; page with --offset/--limit",
            start + 1,
            end,
            proceedings.len()
        ));
    }
}

fn scotus_case_name(v: &Value) -> Option<String> {
    // "Loper Bright Enterprises, et al., Petitioners" / "Gina Raimondo,
    // Secretary of Commerce, et al." -> "Loper Bright Enterprises, et al. v.
    // Gina Raimondo, Secretary of Commerce, et al."
    let pet = str_of(v, "PetitionerTitle").map(|s| trim_party_role(&s));
    let resp = str_of(v, "RespondentTitle").map(|s| trim_party_role(&s));
    match (pet, resp) {
        (Some(p), Some(r)) => Some(format!("{p} v. {r}")),
        (Some(p), None) => Some(p),
        (None, r) => r,
    }
}

/// Drop the trailing procedural role the Court appends to party captions.
fn trim_party_role(title: &str) -> String {
    let t = title.trim().trim_end_matches(',').trim();
    for role in [
        ", Petitioners",
        ", Petitioner",
        ", Respondents",
        ", Respondent",
        ", Applicants",
        ", Applicant",
        ", Appellants",
        ", Appellant",
    ] {
        if let Some(stripped) = t.strip_suffix(role) {
            return stripped.trim_end_matches(',').trim().to_string();
        }
    }
    t.to_string()
}

fn scotus_lower_court(v: &Value) -> Option<String> {
    let court = str_of(v, "LowerCourt")?;
    let mut s = format!("On review from {court}");
    if let Some(nums) = str_of(v, "LowerCourtCaseNumbers") {
        s.push_str(&format!(" {nums}"));
    }
    if let Some(d) = str_of(v, "LowerCourtDecision") {
        s.push_str(&format!(", decided {d}"));
    }
    Some(s)
}

/// A SCOTUS docket closes with judgment/mandate language; anything else means
/// the case is still live, so we return None rather than guessing.
fn scotus_terminated(proceedings: &[Value]) -> Option<String> {
    let last = proceedings.last()?;
    let text = str_of(last, "Text")?.to_ascii_lowercase();
    let closing = ["judgment issued", "mandate issued", "case removed from the docket"];
    closing
        .iter()
        .any(|k| text.contains(k))
        .then(|| str_of(last, "Date").and_then(|d| iso_date(&d)))
        .flatten()
}

// ── CourtListener: by docket id ────────────────────────────────────────────

async fn by_docket_id(id: u64, req: &DocketRequest, out: &mut DocketResponse) {
    out.docket_id = Some(id);
    if cl::has_token() {
        if let Some(v) = cl::get(&format!("dockets/{id}/"), &mut out.warnings).await {
            apply_docket_object(&v, out);
            out.source = Some("courtlistener (token)".to_string());
            if req.include_entries {
                token_entries(id, req, out).await;
            }
            return;
        }
        out.warnings.push(format!(
            "courtlistener /dockets/{id}/ did not answer; falling back to the key-free RECAP \
             search path, which sees only filings that have been indexed."
        ));
    }
    // Key-free: `q=docket_id:<id>` is a fielded Elasticsearch term, measured
    // working anonymously on both type=r (docket metadata) and type=rd
    // (per-filing rows). It is the whole reason this tool answers without a
    // token.
    by_search(&format!("docket_id:{id}"), Some(id), req, out).await;
}

// ── CourtListener: by court + docket number ────────────────────────────────

async fn by_court_and_number(
    court: &str,
    number: &str,
    req: &DocketRequest,
    out: &mut DocketResponse,
) {
    let court = court.trim().to_ascii_lowercase();
    if cl::has_token() {
        let path = format!(
            "dockets/?court={}&docket_number={}",
            urlencoding::encode(&court),
            urlencoding::encode(number.trim())
        );
        if let Some(v) = cl::get(&path, &mut out.warnings).await {
            let rows = v
                .get("results")
                .and_then(|r| r.as_array())
                .cloned()
                .unwrap_or_default();
            match rows.len() {
                0 => out.warnings.push(format!(
                    "courtlistener has no docket {number} in {court}. The docket number filter is \
                     exact here; the RECAP search fallback fuzzy-matches, so it is tried next."
                )),
                _ => {
                    // Duplicate ingests of one case are common; take the
                    // highest id (the most recently merged record) and list the
                    // rest so the caller can see the split.
                    let mut idx = 0usize;
                    for (i, r) in rows.iter().enumerate() {
                        if u64_of(r, "id") > u64_of(&rows[idx], "id") {
                            idx = i;
                        }
                    }
                    if rows.len() > 1 {
                        out.warnings.push(format!(
                            "courtlistener holds {} separate docket records for {court} {number} \
                             (duplicate RECAP ingests); using the newest. The others are in \
                             `candidates`.",
                            rows.len()
                        ));
                        for (i, r) in rows.iter().enumerate() {
                            if i != idx {
                                out.candidates.push(slim_docket_object(r));
                            }
                        }
                    }
                    apply_docket_object(&rows[idx], out);
                    out.source = Some("courtlistener (token)".to_string());
                    if let (true, Some(id)) = (req.include_entries, out.docket_id) {
                        sleep().await;
                        token_entries(id, req, out).await;
                    }
                    return;
                }
            }
        }
    }
    by_search_court_number(&court, number.trim(), req, out).await;
}

// ── CourtListener: the key-free RECAP search paths ─────────────────────────

/// `type=r` filtered by the sidebar params, which are accepted anonymously with
/// no `q` at all (measured). Returns dockets with up to three nested filings.
async fn by_search_court_number(
    court: &str,
    number: &str,
    req: &DocketRequest,
    out: &mut DocketResponse,
) {
    let path = format!(
        "search/?type=r&court={}&docket_number={}",
        urlencoding::encode(court),
        urlencoding::encode(number)
    );
    let Some(v) = cl::get(&path, &mut out.warnings).await else {
        return;
    };
    let cands = candidates_from_search(&v);
    finish_from_candidates(cands, Some(number), req, out).await;
}

/// Free-text or fielded `q` against `type=r`. Used for the query-only path and
/// as the key-free fallback for an explicit docket id.
async fn by_search(q: &str, known_id: Option<u64>, req: &DocketRequest, out: &mut DocketResponse) {
    let mut path = format!("search/?type=r&q={}", urlencoding::encode(q));
    if let Some(c) = req.court.as_deref().filter(|c| !c.trim().is_empty()) {
        path.push_str(&format!("&court={}", urlencoding::encode(c.trim())));
    }
    let Some(v) = cl::get(&path, &mut out.warnings).await else {
        return;
    };
    let mut cands = candidates_from_search(&v);
    // When the caller named a docket id, the only acceptable match is that id.
    if let Some(id) = known_id {
        cands.retain(|c| c.docket_id == Some(id));
        if cands.is_empty() {
            out.warnings.push(format!(
                "docket {id} is not reachable without a token: /dockets/ is 401 anonymously and \
                 RECAP search returned no docket with that id. Set COURTLISTENER_TOKEN (free at \
                 courtlistener.com/profile/api-token/)."
            ));
            return;
        }
    }
    finish_from_candidates(cands, req.docket_number.as_deref(), req, out).await;
}

/// Turn a candidate list into an answer: pick one, report duplicates, or hand
/// the caller the list and refuse to guess.
async fn finish_from_candidates(
    cands: Vec<Candidate>,
    wanted_number: Option<&str>,
    req: &DocketRequest,
    out: &mut DocketResponse,
) {
    if cands.is_empty() {
        out.warnings
            .push("no docket matched. RECAP only covers federal (PACER) courts — state dockets are not in it.".to_string());
        return;
    }
    out.source = Some("courtlistener recap search (key-free)".to_string());

    match choose_candidate(&cands, wanted_number) {
        Choice::Ambiguous => {
            out.warnings.push(format!(
                "{} dockets matched and none is an unambiguous answer; they are listed in \
                 `candidates`. Re-run with --court and --number, or --id, to pick one.",
                cands.len()
            ));
            out.candidates = cands.iter().map(Candidate::to_json).collect();
        }
        Choice::Duplicates(best, n) => {
            out.warnings.push(format!(
                "courtlistener holds {n} docket records for this case (duplicate RECAP ingests); \
                 using the one with the most filings indexed. The others are in `candidates`.",
            ));
            for (i, c) in cands.iter().enumerate() {
                if i != best {
                    out.candidates.push(c.to_json());
                }
            }
            adopt(&cands[best], req, out).await;
        }
        Choice::One(idx) => {
            adopt(&cands[idx], req, out).await;
        }
    }
}

async fn adopt(c: &Candidate, req: &DocketRequest, out: &mut DocketResponse) {
    out.docket_id = c.docket_id;
    out.case_name = c.case_name.clone();
    out.court = c.court.clone();
    out.docket_number = c.docket_number.clone();
    out.date_filed = c.date_filed.clone();
    out.date_terminated = c.date_terminated.clone();
    out.nature_of_suit = c.nature_of_suit.clone();
    out.cause = c.cause.clone();
    out.assigned_to = c.assigned_to.clone();
    out.jury_demand = c.jury_demand.clone();
    out.url = c.absolute_url.clone();

    if !req.include_entries {
        return;
    }
    // A token, if present, gets the real docket sheet; without one, walk
    // type=rd, which is the only anonymous route to per-filing rows.
    if let Some(id) = c.docket_id {
        sleep().await;
        if cl::has_token() {
            token_entries(id, req, out).await;
            if !out.entries.is_empty() {
                out.source = Some("courtlistener (token)".to_string());
                return;
            }
        }
        search_entries(id, c, req, out).await;
    }
}

/// Walk `type=rd&q=docket_id:<id>` to build a docket sheet without a token.
///
/// The rows are *documents*, not entries, so they are grouped by entry number
/// on the way out. Upstream's `count` is therefore a document count, and the
/// warning below says so rather than letting it masquerade as an entry total.
async fn search_entries(id: u64, c: &Candidate, req: &DocketRequest, out: &mut DocketResponse) {
    let court = c.court.clone().unwrap_or_default();
    let pacer_case_id = c.pacer_case_id.clone();
    let mut path = format!(
        "search/?type=rd&q={}&order_by=entry_date_filed%20asc",
        urlencoding::encode(&format!("docket_id:{id}"))
    );
    let mut docs: Vec<Value> = Vec::new();
    let mut reported: Option<usize> = None;
    let want = req.offset.saturating_add(req.limit.max(1));
    let pages_needed = want.div_ceil(SEARCH_PAGE).clamp(1, MAX_SEARCH_PAGES);

    for page in 0..pages_needed {
        if page > 0 {
            sleep().await;
        }
        let Some(v) = cl::get(&path, &mut out.warnings).await else {
            break;
        };
        if reported.is_none() {
            reported = v.get("count").and_then(|x| x.as_u64()).map(|n| n as usize);
        }
        if let Some(arr) = v.get("results").and_then(|r| r.as_array()) {
            docs.extend(arr.iter().cloned());
        }
        match v.get("next").and_then(|n| n.as_str()) {
            Some(next) => path = relative_cl_path(next),
            None => break,
        }
    }

    if docs.is_empty() {
        out.warnings.push(format!(
            "no filings indexed for docket {id}. Without a token only documents already in the \
             RECAP Archive are visible; entries nobody has purchased from PACER are invisible \
             here. Set COURTLISTENER_TOKEN for the authoritative docket sheet."
        ));
        // Fall back to the ≤3 filings the type=r result already carried, so we
        // return something rather than nothing.
        out.entries = c.entries.clone();
        out.entry_count = c.entries.len();
        return;
    }

    let all = entries_from_documents(&docs, &court, pacer_case_id.as_deref());
    // `entry_count` must describe the whole docket, not this window. When we
    // exhausted the result set the grouped count is exact; when we stopped at
    // the page cap, the document total is the honest upper bound we have.
    let exhausted = docs.len() >= reported.unwrap_or(usize::MAX);
    out.entry_count = if exhausted { all.len() } else { reported.unwrap_or(all.len()) };
    let (start, end) = page_window(all.len(), req.offset, req.limit);
    out.entries = all[start..end].to_vec();

    let mut note = format!(
        "key-free path: RECAP search reports {} indexed filings for docket {id}",
        reported.unwrap_or(docs.len())
    );
    if !exhausted {
        note.push_str(&format!(
            "; stopped after {} of them to stay inside the anonymous rate limit, so `entry_count` \
             counts filings rather than docket-sheet rows",
            docs.len()
        ));
    }
    note.push_str(". A token switches this to /docket-entries/, which is the authoritative sheet.");
    out.warnings.push(note);
}

// ── CourtListener: the token docket-entry path ─────────────────────────────

async fn token_entries(id: u64, req: &DocketRequest, out: &mut DocketResponse) {
    let court = out.court.clone().unwrap_or_default();
    // recap_sequence_number is the canonical docket-sheet order; `id` is the
    // tie-breaker the docs require, and neither triggers cursor mode, so plain
    // `page=` keeps working.
    let base = format!("docket-entries/?docket={id}&order_by=recap_sequence_number,id&page_size=100");
    let mut all: Vec<Value> = Vec::new();
    let mut reported: Option<usize> = None;
    let want = req.offset.saturating_add(req.limit.max(1));

    for page in 1..=MAX_ENTRY_PAGES {
        if page > 1 {
            sleep().await;
        }
        let Some(v) = cl::get(&format!("{base}&page={page}"), &mut out.warnings).await else {
            break;
        };
        if reported.is_none() {
            // On the DB endpoints `count` is a URL string, not a number,
            // unless you ask for it explicitly.
            reported = v.get("count").and_then(|x| x.as_u64()).map(|n| n as usize);
        }
        let n = match v.get("results").and_then(|r| r.as_array()) {
            Some(arr) => {
                all.extend(arr.iter().cloned());
                arr.len()
            }
            None => 0,
        };
        let more = v.get("next").and_then(|x| x.as_str()).is_some();
        if n == 0 || !more || all.len() >= want {
            break;
        }
        if page == MAX_ENTRY_PAGES {
            out.warnings.push(format!(
                "stopped after {MAX_ENTRY_PAGES} pages ({} entries) — CourtListener allows 5 \
                 requests/minute, so deeper paging would stall. Narrow with --offset/--limit.",
                all.len()
            ));
        }
    }

    if reported.is_none() {
        // The paginated envelope handed back a URL instead of a number; one
        // extra `count=on` call is the documented way to get the real total,
        // and it is cheap (no results are serialized).
        sleep().await;
        if let Some(v) = cl::get(&format!("docket-entries/?docket={id}&count=on"), &mut out.warnings).await {
            reported = v.get("count").and_then(|x| x.as_u64()).map(|n| n as usize);
        }
    }

    let entries: Vec<DocketEntry> = all
        .iter()
        .map(|e| parse_docket_entry(e, &court))
        .collect();
    out.entry_count = reported.unwrap_or(entries.len());
    let (start, end) = page_window(entries.len(), req.offset, req.limit);
    out.entries = entries[start..end].to_vec();
    if out.entry_count > out.entries.len() {
        out.warnings.push(format!(
            "showing {} of {} docket entries; page with --offset/--limit",
            out.entries.len(),
            out.entry_count
        ));
    }
}

fn parse_docket_entry(e: &Value, court: &str) -> DocketEntry {
    let documents = e
        .get("recap_documents")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .map(|d| RecapDocument {
                    document_number: number_or_string(d, "document_number"),
                    attachment_number: d
                        .get("attachment_number")
                        .and_then(|x| x.as_u64())
                        .map(|n| n as u32),
                    description: str_of(d, "description").or_else(|| str_of(d, "short_description")),
                    page_count: d.get("page_count").and_then(|x| x.as_u64()).map(|n| n as u32),
                    is_available: d
                        .get("is_available")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false),
                    filepath_local: str_of(d, "filepath_local").and_then(|p| storage_url(&p)),
                    pacer_url: pacer_doc_url(court, str_of(d, "pacer_doc_id").as_deref()),
                    // The text itself is enormous and the caller did not ask
                    // for it; its size is still worth reporting so a caller
                    // knows whether a full-text fetch is worthwhile.
                    plain_text_chars: str_of(d, "plain_text").map(|t| t.chars().count()),
                })
                .collect()
        })
        .unwrap_or_default();
    DocketEntry {
        entry_number: e.get("entry_number").and_then(|x| x.as_u64()),
        date_filed: str_of(e, "date_filed"),
        description: str_of(e, "description"),
        documents,
    }
}

fn apply_docket_object(v: &Value, out: &mut DocketResponse) {
    out.docket_id = v.get("id").and_then(|x| x.as_u64()).or(out.docket_id);
    out.case_name = str_of(v, "case_name").or_else(|| str_of(v, "case_name_full"));
    out.court = str_of(v, "court_id").or_else(|| out.court.clone());
    out.docket_number = str_of(v, "docket_number").or_else(|| out.docket_number.clone());
    out.date_filed = str_of(v, "date_filed");
    out.date_terminated = str_of(v, "date_terminated");
    out.nature_of_suit = str_of(v, "nature_of_suit");
    out.cause = str_of(v, "cause");
    out.assigned_to = str_of(v, "assigned_to_str");
    out.jury_demand = str_of(v, "jury_demand");
    out.url = str_of(v, "absolute_url").map(|u| absolute_cl_url(&u));
}

fn slim_docket_object(v: &Value) -> Value {
    serde_json::json!({
        "docket_id": v.get("id").and_then(|x| x.as_u64()),
        "court": str_of(v, "court_id"),
        "case_name": str_of(v, "case_name"),
        "docket_number": str_of(v, "docket_number"),
        "date_filed": str_of(v, "date_filed"),
        "date_terminated": str_of(v, "date_terminated"),
        "url": str_of(v, "absolute_url").map(|u| absolute_cl_url(&u)),
    })
}

// ── candidate model and disambiguation ─────────────────────────────────────

#[derive(Clone, Debug, Default)]
struct Candidate {
    docket_id: Option<u64>,
    court: Option<String>,
    case_name: Option<String>,
    docket_number: Option<String>,
    date_filed: Option<String>,
    date_terminated: Option<String>,
    nature_of_suit: Option<String>,
    cause: Option<String>,
    assigned_to: Option<String>,
    jury_demand: Option<String>,
    pacer_case_id: Option<String>,
    absolute_url: Option<String>,
    /// Highest entry number among the (at most three) filings the search
    /// nested under this docket. It is the only free signal we get for which
    /// of several duplicate records is the most complete.
    max_entry_number: Option<u64>,
    entries: Vec<DocketEntry>,
}

impl Candidate {
    fn to_json(&self) -> Value {
        serde_json::json!({
            "docket_id": self.docket_id,
            "court": self.court,
            "case_name": self.case_name,
            "docket_number": self.docket_number,
            "date_filed": self.date_filed,
            "date_terminated": self.date_terminated,
            "assigned_to": self.assigned_to,
            "nature_of_suit": self.nature_of_suit,
            "max_entry_number_seen": self.max_entry_number,
            "url": self.absolute_url,
        })
    }
}

#[derive(Debug, PartialEq)]
enum Choice {
    /// Exactly one docket answers the request.
    One(usize),
    /// Several records, but all the same case — pick one, disclose the rest.
    Duplicates(usize, usize),
    /// Genuinely different cases; the caller has to choose.
    Ambiguous,
}

/// Decide whether a candidate list is an answer or a menu.
///
/// The distinction that matters: CourtListener routinely holds several docket
/// records for one case (separate RECAP ingests of the same PACER docket),
/// and those are *not* an ambiguity — merging them into one answer with the
/// duplicates disclosed is what the user wants. Two different cases that
/// happen to match the query are a real ambiguity, and guessing between them
/// is exactly the silent-wrong-answer failure this tool exists to avoid.
fn choose_candidate(cands: &[Candidate], wanted_number: Option<&str>) -> Choice {
    if cands.is_empty() {
        return Choice::Ambiguous;
    }
    // Narrow to exact docket-number matches when the caller supplied one.
    let idxs: Vec<usize> = match wanted_number.map(normalize_docket_number) {
        Some(want) if !want.is_empty() => {
            let exact: Vec<usize> = (0..cands.len())
                .filter(|&i| {
                    cands[i]
                        .docket_number
                        .as_deref()
                        .map(|n| normalize_docket_number(n) == want)
                        .unwrap_or(false)
                })
                .collect();
            if exact.is_empty() {
                (0..cands.len()).collect()
            } else {
                exact
            }
        }
        _ => (0..cands.len()).collect(),
    };

    if idxs.len() == 1 {
        return Choice::One(idxs[0]);
    }
    let same_case = idxs.windows(2).all(|w| {
        let (a, b) = (&cands[w[0]], &cands[w[1]]);
        a.court == b.court
            && a.docket_number.as_deref().map(normalize_docket_number)
                == b.docket_number.as_deref().map(normalize_docket_number)
            && a.docket_number.is_some()
    });
    if !same_case {
        return Choice::Ambiguous;
    }
    // Fullest record wins; ties break to the highest id, which is the most
    // recently merged ingest.
    let best = idxs
        .iter()
        .copied()
        .max_by_key(|&i| (cands[i].max_entry_number.unwrap_or(0), cands[i].docket_id.unwrap_or(0)))
        .unwrap_or(idxs[0]);
    Choice::Duplicates(best, idxs.len())
}

fn candidates_from_search(v: &Value) -> Vec<Candidate> {
    let Some(rows) = v.get("results").and_then(|r| r.as_array()) else {
        return Vec::new();
    };
    rows.iter()
        .map(|r| {
            let court = str_of(r, "court_id");
            let pacer_case_id = str_of(r, "pacer_case_id");
            let docs = r
                .get("recap_documents")
                .and_then(|d| d.as_array())
                .cloned()
                .unwrap_or_default();
            let max_entry_number = docs
                .iter()
                .filter_map(|d| d.get("entry_number").and_then(|x| x.as_u64()))
                .max();
            Candidate {
                docket_id: r.get("docket_id").and_then(|x| x.as_u64()),
                case_name: str_of(r, "caseName").or_else(|| str_of(r, "case_name_full")),
                docket_number: str_of(r, "docketNumber"),
                date_filed: str_of(r, "dateFiled"),
                date_terminated: str_of(r, "dateTerminated"),
                nature_of_suit: str_of(r, "suitNature"),
                cause: str_of(r, "cause"),
                assigned_to: str_of(r, "assignedTo"),
                jury_demand: str_of(r, "juryDemand"),
                absolute_url: str_of(r, "docket_absolute_url").map(|u| absolute_cl_url(&u)),
                entries: entries_from_documents(&docs, court.as_deref().unwrap_or(""), pacer_case_id.as_deref()),
                max_entry_number,
                pacer_case_id,
                court,
            }
        })
        .collect()
}

/// Regroup flat RECAP *document* rows into docket-sheet *entries*.
///
/// The search API only ever returns documents; the docket sheet is entries. One
/// entry holds the main filing plus its attachments, so grouping by
/// `entry_number` reconstructs the row a user would see on PACER. Rows with no
/// entry number (minute entries, stray attachments) are kept in their own
/// trailing bucket rather than dropped — a missing row is worse than an
/// unnumbered one.
fn entries_from_documents(docs: &[Value], court: &str, pacer_case_id: Option<&str>) -> Vec<DocketEntry> {
    let mut entries: Vec<DocketEntry> = Vec::new();
    for d in docs {
        let entry_number = d.get("entry_number").and_then(|x| x.as_u64());
        let date_filed = str_of(d, "entry_date_filed");
        let doc = RecapDocument {
            document_number: number_or_string(d, "document_number"),
            attachment_number: d
                .get("attachment_number")
                .and_then(|x| x.as_u64())
                .map(|n| n as u32),
            description: str_of(d, "short_description").or_else(|| str_of(d, "description")),
            page_count: d.get("page_count").and_then(|x| x.as_u64()).map(|n| n as u32),
            is_available: d.get("is_available").and_then(|x| x.as_bool()).unwrap_or(false),
            filepath_local: str_of(d, "filepath_local").and_then(|p| storage_url(&p)),
            pacer_url: pacer_doc_url(court, str_of(d, "pacer_doc_id").as_deref()),
            plain_text_chars: None,
        };
        // The long clerk text lives on the document row in search results; the
        // entry description is the same string, so take the longest one seen.
        let long_desc = str_of(d, "description");
        match entries
            .iter_mut()
            .find(|e| e.entry_number.is_some() && e.entry_number == entry_number)
        {
            Some(e) => {
                if let Some(t) = long_desc {
                    if e.description.as_ref().map(|x| x.len()).unwrap_or(0) < t.len() {
                        e.description = Some(t);
                    }
                }
                e.documents.push(doc);
            }
            None => entries.push(DocketEntry {
                entry_number,
                date_filed,
                description: long_desc,
                documents: vec![doc],
            }),
        }
    }
    // Docket-sheet order: numbered entries ascending, unnumbered rows last.
    entries.sort_by_key(|e| (e.entry_number.is_none(), e.entry_number.unwrap_or(u64::MAX)));
    let _ = pacer_case_id; // kept for symmetry with the docket-level PACER link
    entries
}

// ── helpers ───────────────────────────────────────────────────────────────

/// `filepath_local` is a storage path, not a URL. `storage.courtlistener.com`
/// serves it with no auth and outside the API throttle, so turning it absolute
/// here is the difference between a field the caller can use and one it can't.
fn storage_url(path: &str) -> Option<String> {
    let p = path.trim();
    if p.is_empty() {
        return None;
    }
    if p.starts_with("http://") || p.starts_with("https://") {
        return Some(p.to_string());
    }
    Some(format!("{CL_STORAGE}/{}", p.trim_start_matches('/')))
}

/// PACER's per-document viewer. Only meaningful for the federal courts whose
/// ECF hostname is derived from the court id.
fn pacer_doc_url(court: &str, pacer_doc_id: Option<&str>) -> Option<String> {
    let id = pacer_doc_id?.trim();
    let court = court.trim().to_ascii_lowercase();
    if id.is_empty() || court.is_empty() || court == "scotus" {
        return None;
    }
    Some(format!("https://ecf.{court}.uscourts.gov/doc1/{id}"))
}

fn absolute_cl_url(path: &str) -> String {
    if path.starts_with("http") {
        path.to_string()
    } else {
        format!("{}{}", cl::CL_WEB, path)
    }
}

/// The search API's `next` is an absolute URL; `cl::get` wants a path relative
/// to the v4 base, and the cursor token must survive verbatim.
fn relative_cl_path(next: &str) -> String {
    match next.split_once("/api/rest/v4/") {
        Some((_, rest)) => rest.to_string(),
        None => next.to_string(),
    }
}

/// Compare docket numbers the way a human does: "1:22-cr-00673-KPF",
/// "22-cr-673" and "1:22-CR-00673" are the same case. The office prefix, the
/// zero padding and the trailing judge initials are all presentation.
fn normalize_docket_number(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();
    // Drop anything after the first whitespace — "(KPF)", "et al", stray notes.
    let head = lowered.split_whitespace().next().unwrap_or("");
    // Everything before the last ':' is the divisional office prefix.
    let core = head.rsplit(':').next().unwrap_or(head);
    let mut segs: Vec<String> = core
        .split('-')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.chars().all(|c| c.is_ascii_digit()) {
                let trimmed = s.trim_start_matches('0');
                if trimmed.is_empty() { "0".to_string() } else { trimmed.to_string() }
            } else {
                s.to_string()
            }
        })
        .collect();
    // Trailing all-alphabetic segments are judge initials, not identity.
    while segs.len() > 2 && segs.last().is_some_and(|s| s.chars().all(|c| c.is_ascii_alphabetic())) {
        segs.pop();
    }
    segs.join("-")
}

/// Half-open window over a list, clamped so a wild --offset returns an empty
/// page rather than panicking or silently wrapping to the start.
fn page_window(len: usize, offset: usize, limit: usize) -> (usize, usize) {
    let start = offset.min(len);
    let end = if limit == 0 { len } else { start.saturating_add(limit).min(len) };
    (start, end)
}

/// Several date formats show up on supremecourt.gov: "Nov 10 2022" on
/// proceedings, "November 15, 2022" on the docketed date.
fn iso_date(raw: &str) -> Option<String> {
    let s = raw.trim();
    for fmt in ["%b %d %Y", "%B %d, %Y", "%b %d, %Y", "%B %d %Y", "%Y-%m-%d"] {
        if let Ok(d) = chrono::NaiveDate::parse_from_str(s, fmt) {
            return Some(d.to_string());
        }
    }
    None
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn u64_of(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// `document_number` is an integer on the search API and a string on the DB
/// API. Same field, two encodings.
fn number_or_string(v: &Value, key: &str) -> Option<String> {
    match v.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

async fn sleep() {
    tokio::time::sleep(PAGE_DELAY).await;
}

async fn get_json(url: &str, source: &str, warnings: &mut Vec<String>) -> Option<Value> {
    let resp = match shared_client::GENERAL.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("{source} request failed: {e}"));
            return None;
        }
    };
    let resp = soft_fail(source, resp, warnings).await?;
    match resp.json::<Value>().await {
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

    fn cand(id: u64, court: &str, number: &str, max_entry: Option<u64>) -> Candidate {
        Candidate {
            docket_id: Some(id),
            court: Some(court.to_string()),
            docket_number: Some(number.to_string()),
            max_entry_number: max_entry,
            ..Candidate::default()
        }
    }

    #[test]
    fn normalizes_docket_numbers_across_presentations() {
        assert_eq!(normalize_docket_number("1:22-cr-00673"), "22-cr-673");
        assert_eq!(normalize_docket_number("1:22-cr-00673-KPF"), "22-cr-673");
        assert_eq!(normalize_docket_number("22-CR-673"), "22-cr-673");
        assert_eq!(normalize_docket_number(" 1:22-cr-00673 (LAK) "), "22-cr-673");
        assert_eq!(normalize_docket_number("1:16-cv-00745"), "16-cv-745");
        // Two-segment numbers keep their tail: nothing to mistake for initials.
        assert_eq!(normalize_docket_number("22-451"), "22-451");
    }

    #[test]
    fn detects_supreme_court_docket_numbers() {
        assert!(is_scotus_docket_number("22-451"));
        assert!(is_scotus_docket_number("23A994"));
        assert!(is_scotus_docket_number("22o145"));
        assert!(is_scotus_docket_number("24M12"));
        // District and circuit numbers must not be swallowed by this.
        assert!(!is_scotus_docket_number("1:22-cr-00673"));
        assert!(!is_scotus_docket_number("22-cr-673"));
        assert!(!is_scotus_docket_number("2:19-cv-1234"));
        assert!(!is_scotus_docket_number(""));
        assert!(!is_scotus_docket_number("451"));
    }

    #[test]
    fn routes_to_scotus_only_when_it_should() {
        assert_eq!(scotus_target(Some("scotus"), Some("22-451")).as_deref(), Some("22-451"));
        assert_eq!(scotus_target(Some("SCOTUS"), Some("23a994")).as_deref(), Some("23A994"));
        // Court named and it is not SCOTUS: never route here, even for a
        // number that happens to look like one (ca9 uses "23-3018").
        assert_eq!(scotus_target(Some("ca9"), Some("23-3018")), None);
        // No court at all: the shape is the only signal we have.
        assert_eq!(scotus_target(None, Some("22-451")).as_deref(), Some("22-451"));
        assert_eq!(scotus_target(None, Some("1:22-cr-00673")), None);
        assert_eq!(scotus_target(Some("scotus"), None), None);
    }

    #[test]
    fn turns_relative_storage_paths_absolute() {
        assert_eq!(
            storage_url("recap/gov.uscourts.nysd.605906/gov.uscourts.nysd.605906.553.0.pdf")
                .as_deref(),
            Some("https://storage.courtlistener.com/recap/gov.uscourts.nysd.605906/gov.uscourts.nysd.605906.553.0.pdf")
        );
        // Leading slash must not produce a double slash.
        assert_eq!(
            storage_url("/recap/x.pdf").as_deref(),
            Some("https://storage.courtlistener.com/recap/x.pdf")
        );
        // supremecourt.gov already hands us absolute URLs.
        assert_eq!(
            storage_url("https://www.supremecourt.gov/DocketPDF/22/22-451/1.pdf").as_deref(),
            Some("https://www.supremecourt.gov/DocketPDF/22/22-451/1.pdf")
        );
        assert_eq!(storage_url("  "), None);
    }

    #[test]
    fn builds_pacer_document_links() {
        assert_eq!(
            pacer_doc_url("nysd", Some("127036927794")).as_deref(),
            Some("https://ecf.nysd.uscourts.gov/doc1/127036927794")
        );
        assert_eq!(pacer_doc_url("nysd", None), None);
        // SCOTUS is not on PACER at all.
        assert_eq!(pacer_doc_url("scotus", Some("1")), None);
    }

    #[test]
    fn entry_paging_math_is_clamped() {
        assert_eq!(page_window(10, 0, 4), (0, 4));
        assert_eq!(page_window(10, 3, 4), (3, 7));
        // Limit past the end truncates instead of overrunning.
        assert_eq!(page_window(10, 8, 50), (8, 10));
        // Offset past the end is an empty page, not a panic.
        assert_eq!(page_window(10, 99, 5), (10, 10));
        // limit 0 means "everything from offset".
        assert_eq!(page_window(10, 2, 0), (2, 10));
        assert_eq!(page_window(0, 0, 20), (0, 0));
    }

    #[test]
    fn picks_the_fullest_of_duplicate_docket_records() {
        // Measured shape: CourtListener holds six records for nysd
        // 1:22-cr-00673, all the same case.
        let cands = vec![
            cand(66631291, "nysd", "1:22-cr-00673", Some(4)),
            cand(67772540, "nysd", "1:22-cr-00673", Some(553)),
            cand(66907121, "nysd", "1:22-cr-00673", Some(120)),
        ];
        assert_eq!(
            choose_candidate(&cands, Some("1:22-cr-00673")),
            Choice::Duplicates(1, 3)
        );
    }

    #[test]
    fn refuses_to_guess_between_different_cases() {
        let cands = vec![
            cand(1, "nysd", "1:22-cr-00673", Some(10)),
            cand(2, "cand", "3:23-cv-00099", Some(10)),
        ];
        assert_eq!(choose_candidate(&cands, None), Choice::Ambiguous);
        assert_eq!(choose_candidate(&[], None), Choice::Ambiguous);
    }

    #[test]
    fn an_exact_number_narrows_a_mixed_candidate_list() {
        let cands = vec![
            cand(1, "nysd", "1:22-cv-09500", Some(3)),
            cand(2, "nysd", "1:22-cr-00673", Some(9)),
            cand(3, "nysd", "1:23-cr-00118", Some(3)),
        ];
        assert_eq!(choose_candidate(&cands, Some("22-cr-673")), Choice::One(1));
        // A single candidate is always the answer, number or not.
        assert_eq!(choose_candidate(&cands[..1], None), Choice::One(0));
    }

    #[test]
    fn groups_flat_document_rows_into_docket_entries() {
        let docs: Vec<Value> = serde_json::from_str(
            r#"[
              {"entry_number": 553, "entry_date_filed": "2025-02-05", "description": "MEMO ENDORSEMENT as to Samuel Bankman-Fried",
               "short_description": "Memo Endorsement", "document_number": 553, "is_available": true,
               "page_count": 1, "pacer_doc_id": "127036927794",
               "filepath_local": "recap/gov.uscourts.nysd.605906/x.553.0.pdf"},
              {"entry_number": 553, "entry_date_filed": "2025-02-05", "attachment_number": 1,
               "short_description": "Exhibit A", "document_number": 553, "is_available": false},
              {"entry_number": 1, "entry_date_filed": "2022-12-09", "short_description": "Indictment",
               "document_number": 1, "is_available": true},
              {"entry_date_filed": "2022-12-13", "short_description": "Minute Entry", "is_available": false}
            ]"#,
        )
        .expect("fixture parses");
        let entries = entries_from_documents(&docs, "nysd", Some("605906"));
        assert_eq!(entries.len(), 3);
        // Numbered entries ascending, unnumbered last.
        assert_eq!(entries[0].entry_number, Some(1));
        assert_eq!(entries[1].entry_number, Some(553));
        assert_eq!(entries[2].entry_number, None);
        // Attachments fold into their parent entry.
        assert_eq!(entries[1].documents.len(), 2);
        assert_eq!(entries[1].documents[1].attachment_number, Some(1));
        // The long clerk text wins over the short label.
        assert!(entries[1]
            .description
            .as_deref()
            .unwrap_or("")
            .starts_with("MEMO ENDORSEMENT"));
        assert_eq!(
            entries[1].documents[0].filepath_local.as_deref(),
            Some("https://storage.courtlistener.com/recap/gov.uscourts.nysd.605906/x.553.0.pdf")
        );
        assert_eq!(
            entries[1].documents[0].pacer_url.as_deref(),
            Some("https://ecf.nysd.uscourts.gov/doc1/127036927794")
        );
        // Integer document_number from search, not a string.
        assert_eq!(entries[0].documents[0].document_number.as_deref(), Some("1"));
    }

    #[test]
    fn parses_supreme_court_date_formats() {
        assert_eq!(iso_date("Nov 10 2022").as_deref(), Some("2022-11-10"));
        assert_eq!(iso_date("November 15, 2022").as_deref(), Some("2022-11-15"));
        assert_eq!(iso_date("2022-11-15").as_deref(), Some("2022-11-15"));
        assert_eq!(iso_date("not a date"), None);
    }

    #[test]
    fn builds_a_case_name_from_scotus_party_titles() {
        let v: Value = serde_json::json!({
            "PetitionerTitle": "Loper Bright Enterprises, et al., Petitioners",
            "RespondentTitle": "Gina Raimondo, Secretary of Commerce, et al."
        });
        assert_eq!(
            scotus_case_name(&v).as_deref(),
            Some("Loper Bright Enterprises, et al. v. Gina Raimondo, Secretary of Commerce, et al.")
        );
    }

    #[test]
    fn keeps_search_cursor_tokens_intact_when_relativizing() {
        assert_eq!(
            relative_cl_path(
                "https://www.courtlistener.com/api/rest/v4/search/?cursor=cz0xMzQ%3D&type=rd"
            ),
            "search/?cursor=cz0xMzQ%3D&type=rd"
        );
    }

    #[test]
    fn reads_docket_metadata_off_the_db_object() {
        let v: Value = serde_json::json!({
            "id": 67772540, "court_id": "nysd",
            "case_name": "United States v. Bankman-Fried",
            "docket_number": "1:22-cr-00673", "date_filed": "2022-12-09",
            "date_terminated": "2024-05-29", "nature_of_suit": "",
            "cause": "", "assigned_to_str": "Lewis A. Kaplan", "jury_demand": "None",
            "absolute_url": "/docket/67772540/united-states-v-bankman-fried/"
        });
        let mut out = DocketResponse::empty();
        apply_docket_object(&v, &mut out);
        assert_eq!(out.docket_id, Some(67772540));
        assert_eq!(out.court.as_deref(), Some("nysd"));
        assert_eq!(out.assigned_to.as_deref(), Some("Lewis A. Kaplan"));
        // Empty strings are absent data, not data.
        assert_eq!(out.nature_of_suit, None);
        assert_eq!(
            out.url.as_deref(),
            Some("https://www.courtlistener.com/docket/67772540/united-states-v-bankman-fried/")
        );
    }
}
