//! State appellate-opinion feeds — the non-precedential layer.
//!
//! CourtListener's search returns **Published** opinions by default, and its
//! state coverage of everything else is patchy. But most of what a traffic
//! stop, a small-claims dispute or a run-of-the-mill criminal appeal actually
//! produces is *non-precedential*: an Illinois Rule 23 order, a Pennsylvania
//! Superior Court memorandum, a Michigan unpublished per curiam, a New Jersey
//! unpublished Appellate Division opinion. Those decide the case in front of
//! you and are invisible in the default federal-tool path. Each state's own
//! judiciary publishes them, so this module goes to the source.
//!
//! `unpublished_count` on the response is the headline metric — it is the whole
//! reason this file exists.
//!
//! Four sources, three shapes, all key-free:
//!
//!   * **Illinois** — two RSS feeds (`opinions-appellate`, `opinions-supreme`)
//!     carrying a custom `opinion:` XML namespace, so every field we want is
//!     structured and no HTML parsing is needed. `<category>`/`<opinion:type>`
//!     says "Rule 23" or "Opinion" outright: the cleanest published flag of the
//!     four.
//!   * **Pennsylvania** — three RSS feeds (Superior, Commonwealth, Supreme).
//!     Only Superior encodes precedential status, and it does so in the PDF
//!     *filename* (`J-S25019-26m` = memorandum). See `pa_superior_disposition`.
//!   * **Michigan** — a real JSON search API with a real query parameter and an
//!     `isPublished` boolean. The only one of the four where `req.query` is
//!     answered server-side against the full corpus rather than against a
//!     rolling window.
//!   * **New Jersey** — a Drupal JSON:API. Its opinion-type taxonomy
//!     ("Unpublished Appellate", "Published Appellate", "Supreme", …) is the
//!     authoritative published flag, and `filter[title][operator]=CONTAINS`
//!     gives a second server-side query.
//!
//! **Honest limit, repeated in `warnings` on every RSS answer:** the Illinois
//! and Pennsylvania feeds are a *rolling window* of roughly 50–100 items —
//! weeks, not years. They are a watcher, not a backfill. An empty result from
//! them means "not in the current window", never "no such case".

use super::{StateOpinion, StateOpinionsRequest, StateOpinionsResponse};
use crate::legal::{clamp_text, parse_date, shared_client, soft_fail, strip_markup};
use crate::Result;
use chrono::Utc;
use regex::Regex;
use std::sync::LazyLock;

/// Used when the caller leaves `limit` at zero rather than returning nothing —
/// a zero limit is far more likely to be an unset field than a real request for
/// an empty list.
const DEFAULT_LIMIT: usize = 50;

/// Case captions get long (New Jersey folds county, docket and "RECORD
/// IMPOUNDED" into the title). Clamp so one call can't blow the context window.
const MAX_CASE_NAME: usize = 300;

/// Both Illinois feeds. The appellate one carries the Rule 23 orders; the
/// supreme one is precedential-only but belongs here so "il" means the whole
/// state rather than one court. Verified live: both 200 with 100 items.
const IL_FEEDS: &[(&str, &str)] = &[
    (
        "Appellate Court",
        "https://www.illinoiscourts.gov/views/courts/rss/opinions-appellate.aspx",
    ),
    (
        "Supreme Court",
        "https://www.illinoiscourts.gov/views/courts/rss/opinions-supreme.aspx",
    ),
];

/// All three Pennsylvania appellate courts. Superior is listed first because it
/// is the one that carries the non-precedential memoranda this module is for;
/// the other two are here so "pa" isn't silently one-third of the state.
const PA_FEEDS: &[(&str, &str)] = &[
    (
        "Superior Court",
        "https://www.pacourts.us/Rss/Opinions/Superior/",
    ),
    (
        "Commonwealth Court",
        "https://www.pacourts.us/Rss/Opinions/Commonwealth/",
    ),
    (
        "Supreme Court",
        "https://www.pacourts.us/Rss/Opinions/Supreme/",
    ),
];

const MI_HOST: &str = "https://www.courts.michigan.gov";
const NJ_HOST: &str = "https://www.njcourts.gov";
/// Drupal caps `page[limit]` at 50 regardless of what we ask for (measured:
/// asking 100 returns 50), so paging is the only way past it.
const NJ_PAGE: usize = 50;
/// Bound on how far we'll page New Jersey. 200 items is already far more than
/// any single answer needs and keeps a pathological `limit` from walking 24k
/// records.
const NJ_MAX_PAGES: usize = 4;

pub(super) async fn fetch(req: StateOpinionsRequest) -> Result<StateOpinionsResponse> {
    let code = super::normalize_state(&req.state);
    match code.as_str() {
        "il" => Ok(illinois(&req).await),
        "pa" => Ok(pennsylvania(&req).await),
        "mi" => Ok(michigan(&req).await),
        "nj" => Ok(new_jersey(&req).await),
        // Unreachable through `fetch_state_opinions`, which already gates on
        // the same four codes — but this module is the thing that knows which
        // states it implements, so it answers rather than panicking.
        _ => Err(super::unsupported(&code, "opinions feed", |s| s.opinions)),
    }
}

// ── Illinois ───────────────────────────────────────────────────────────────

async fn illinois(req: &StateOpinionsRequest) -> StateOpinionsResponse {
    let fetched = futures::future::join_all(
        IL_FEEDS
            .iter()
            .map(|(court, url)| get(url, format!("illinoiscourts {court}"))),
    )
    .await;

    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut answered: Vec<&str> = Vec::new();
    for ((court, _url), (body, mut w)) in IL_FEEDS.iter().zip(fetched) {
        warnings.append(&mut w);
        if let Some(xml) = body {
            let parsed = parse_illinois(&xml, court);
            if parsed.is_empty() {
                warnings.push(format!(
                    "illinoiscourts {court} feed answered but held no <item> elements"
                ));
            }
            answered.push(court);
            items.extend(parsed);
        }
    }

    warnings.push(
        "illinoiscourts.gov RSS is a rolling window of 100 items per court (roughly the last few \
         weeks). This is a watcher, not a backfill — an empty result means 'not in the current \
         window', not 'no such case'."
            .to_string(),
    );

    finalize(
        "il",
        req,
        items,
        warnings,
        &format!(
            "Illinois Courts opinions RSS ({})",
            if answered.is_empty() {
                "no feed answered".to_string()
            } else {
                answered.join(" + ")
            }
        ),
        IL_FEEDS[0].1,
        true,
    )
}

/// Illinois publishes a custom namespace — `xmlns:opinion=".../top-level-opinions/"` —
/// with one element per field we want, so this is a straight field lift with no
/// HTML in the way.
fn parse_illinois(xml: &str, court_hint: &str) -> Vec<StateOpinion> {
    rss_items(xml)
        .into_iter()
        .map(|item| {
            // `<category>` and `<opinion:type>` carry the same value; take
            // either, since a feed rebuild could drop one.
            let kind = tag_text(item, "opinion:type")
                .or_else(|| tag_text(item, "category"))
                .unwrap_or_default();
            let citation = tag_text(item, "opinion:citationnum");
            let published = il_published(&kind, citation.as_deref());
            let pdf = tag_text(item, "opinion:pdf").or_else(|| tag_text(item, "link"));
            StateOpinion {
                case_name: tag_text(item, "opinion:casename")
                    .or_else(|| tag_text(item, "title"))
                    .map(|n| clamp_text(&n, MAX_CASE_NAME).0),
                court: tag_text(item, "opinion:court")
                    .unwrap_or_else(|| court_hint.to_string())
                    .into(),
                filed: tag_text(item, "opinion:filingdate")
                    .and_then(|d| us_date(&d))
                    .or_else(|| tag_text(item, "pubDate").and_then(|d| rfc2822_date(&d))),
                citation,
                // The feed carries no docket element. The docket digits are
                // embedded in the citation ("2026 IL App (1st) 242281" is
                // docket 1-24-2281), but reconstructing that formatting is a
                // guess, so report what the source states and nothing more.
                docket: None,
                disposition: il_disposition(&kind),
                published,
                pdf_url: pdf.as_deref().map(encode_spaces),
                url: pdf.as_deref().map(encode_spaces),
            }
        })
        .collect()
}

/// Rule 23 is Illinois' non-precedential disposition — Supreme Court Rule 23
/// orders decide the appeal but may not be cited as precedent. The `-U` /
/// `-UB` citation suffix says the same thing and is checked as a backstop in
/// case the type element is ever blank.
fn il_published(kind: &str, citation: Option<&str>) -> Option<bool> {
    if kind.to_ascii_lowercase().contains("rule 23") {
        return Some(false);
    }
    if citation.is_some_and(|c| c.rsplit(' ').next().is_some_and(|last| last.contains("-U"))) {
        return Some(false);
    }
    if kind.eq_ignore_ascii_case("opinion") {
        return Some(true);
    }
    None
}

fn il_disposition(kind: &str) -> Option<String> {
    match kind.trim() {
        "" => None,
        "Rule 23" => Some("Rule 23 Order".to_string()),
        other => Some(other.to_string()),
    }
}

// ── Pennsylvania ───────────────────────────────────────────────────────────

async fn pennsylvania(req: &StateOpinionsRequest) -> StateOpinionsResponse {
    let fetched = futures::future::join_all(
        PA_FEEDS
            .iter()
            .map(|(court, url)| get(url, format!("pacourts {court}"))),
    )
    .await;

    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut answered: Vec<&str> = Vec::new();
    let mut skipped = 0usize;
    for ((court, _url), (body, mut w)) in PA_FEEDS.iter().zip(fetched) {
        warnings.append(&mut w);
        if let Some(xml) = body {
            let (parsed, dropped) = parse_pennsylvania(&xml, court);
            skipped += dropped;
            answered.push(court);
            items.extend(parsed);
        }
    }

    if skipped > 0 {
        warnings.push(format!(
            "{skipped} pacourts items dropped: the feeds interleave daily judgment lists and \
             weekly reports with the opinions, and those carry no docket number"
        ));
    }
    warnings.push(
        "only the Superior Court feed states precedential status, and it does so in the PDF \
         filename (…m = memorandum, non-precedential; …o = opinion). Commonwealth and Supreme \
         items come back with published: null because their feeds genuinely do not say."
            .to_string(),
    );
    warnings.push(
        "pacourts.us RSS is a rolling window of ~50 items per court (roughly the last week). \
         This is a watcher, not a backfill — an empty result means 'not in the current window', \
         not 'no such case'."
            .to_string(),
    );

    finalize(
        "pa",
        req,
        items,
        warnings,
        &format!(
            "Pennsylvania UJS opinions RSS ({})",
            if answered.is_empty() {
                "no feed answered".to_string()
            } else {
                answered.join(" + ")
            }
        ),
        PA_FEEDS[0].1,
        true,
    )
}

/// Returns the opinions plus a count of items dropped as non-opinions.
///
/// Pennsylvania's feeds are plain RSS with no per-item metadata at all, so
/// everything comes out of two places: the title (case name + docket) and the
/// PDF filename (the disposition code). Daily "Judgment List" PDFs ride the
/// same feed and have neither, which is how they are told apart.
fn parse_pennsylvania(xml: &str, court: &str) -> (Vec<StateOpinion>, usize) {
    let mut out = Vec::new();
    let mut dropped = 0usize;
    for item in rss_items(xml) {
        // The Superior Court puts the docket on its own line inside <title>;
        // collapse it here so one regex serves all three courts.
        let raw_title = tag_raw(item, "title").unwrap_or_default();
        let title = clean(&raw_title.replace(['\n', '\r'], " "));
        let link = tag_text(item, "link");
        let Some((case_name, docket)) = pa_split_title(&title) else {
            dropped += 1;
            continue;
        };
        let file = link
            .as_deref()
            .and_then(|l| l.rsplit('/').next())
            .unwrap_or_default()
            .to_string();
        let (disposition, published) = pa_disposition(court, &file);
        out.push(StateOpinion {
            case_name: Some(clamp_text(&case_name, MAX_CASE_NAME).0),
            court: Some(format!("Pennsylvania {court}")),
            filed: tag_text(item, "pubDate").and_then(|d| rfc2822_date(&d)),
            // PA does not assign a public-domain citation in the feed.
            citation: None,
            docket: Some(docket),
            disposition,
            published,
            pdf_url: link.as_deref().map(encode_spaces),
            url: link.as_deref().map(encode_spaces),
        });
    }
    (out, dropped)
}

/// "Com. v. Alvarez, A. No. 2347 EDA 2025" -> ("Com. v. Alvarez, A.", "2347 EDA 2025").
///
/// All three courts end the title with the docket, differing only in the
/// separator: Superior uses a newline, Commonwealth " - ", Supreme " - No. ".
/// An item with no trailing docket is not an opinion (it is a judgment list).
fn pa_split_title(title: &str) -> Option<(String, String)> {
    static PA_DOCKET: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\s*(?:[-\u{2013}]\s*)?(?:No\.\s*)?(\d+\s+(?:[A-Z]\.?){1,4}\s*\d{4})\s*$")
            .expect("static regex")
    });
    let caps = PA_DOCKET.captures(title)?;
    let whole = caps.get(0)?;
    let name = title[..whole.start()]
        .trim()
        .trim_end_matches([',', '-'])
        .trim();
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), caps.get(1)?.as_str().trim().to_string()))
}

/// Disposition and precedential status, read off the PDF filename.
fn pa_disposition(court: &str, file: &str) -> (Option<String>, Option<bool>) {
    if court.starts_with("Superior") {
        static PA_CODE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^J-[A-Z]\d+-\d+([a-z]*)").expect("static regex"));
        if let Some(c) = PA_CODE.captures(file) {
            return pa_superior_disposition(c.get(1).map(|m| m.as_str()).unwrap_or_default());
        }
        return (None, None);
    }
    // Commonwealth files single-judge orders as "…ORD_8-31-26.pdf". That marks
    // the document type, not precedential status, so `published` stays None.
    if file.to_ascii_uppercase().contains("ORD") {
        return (Some("Order".to_string()), None);
    }
    (None, None)
}

/// Superior Court filenames are `J-<session>-<year><code>`, where the code is
/// the disposition: a trailing `m` is a Rule 65.37 memorandum decision — the
/// non-precedential form, and the bulk of the court's output — and a trailing
/// `o` is a published opinion. A leading letter qualifies it (`dm` dissenting
/// memorandum, `pco` per curiam opinion).
///
/// `jo` (judgment order) is deliberately left as `None`: it is neither of the
/// two, and guessing its precedential status would be inventing a fact the
/// source does not state.
fn pa_superior_disposition(code: &str) -> (Option<String>, Option<bool>) {
    if code.is_empty() {
        return (None, None);
    }
    let (prefix, kind) = code.split_at(code.len() - 1);
    match kind {
        "m" => (
            Some(format!("{}Memorandum", pa_qualifier(prefix))),
            Some(false),
        ),
        "o" if prefix == "j" => (Some("Judgment Order".to_string()), None),
        "o" => (Some(format!("{}Opinion", pa_qualifier(prefix))), Some(true)),
        _ => (None, None),
    }
}

fn pa_qualifier(prefix: &str) -> &'static str {
    match prefix {
        "d" => "Dissenting ",
        "c" => "Concurring ",
        "pc" => "Per Curiam ",
        _ => "",
    }
}

// ── Michigan ───────────────────────────────────────────────────────────────

async fn michigan(req: &StateOpinionsRequest) -> StateOpinionsResponse {
    let limit = effective_limit(req);
    // Michigan is the one source with a real query parameter, so the filter
    // runs against the whole ~82k-opinion corpus rather than a window. Ask for
    // headroom when we're about to drop the published ones client-side.
    let page = limit
        .saturating_mul(if req.unpublished_only { 3 } else { 1 })
        .clamp(25, 200);
    let url = format!(
        "{MI_HOST}/api/CaseSearch/SearchCaseOpinions?searchQuery={}&pageSize={page}",
        urlencoding::encode(req.query.as_deref().unwrap_or(""))
    );

    let (body, mut warnings) = get(&url, "michigan courts opinion search".to_string()).await;
    let mut items = Vec::new();
    if let Some(text) = body {
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(json) => {
                if let Some(total) = json.get("totalResults").and_then(|v| v.as_u64()) {
                    let shown = json
                        .get("searchItems")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0) as u64;
                    if total > shown {
                        warnings.push(format!(
                            "michigan matched {total} opinions; this page carries {shown}. Narrow \
                             with a more specific query."
                        ));
                    }
                }
                items = parse_michigan(&json);
            }
            Err(e) => warnings.push(format!("michigan search parse failed: {e}")),
        }
    }

    if req.query.is_some() {
        warnings.push(
            "michigan filtered server-side via searchQuery, so the match is against the full \
             opinion corpus (not a rolling feed window)."
                .to_string(),
        );
    }

    finalize(
        "mi",
        req,
        items,
        warnings,
        "Michigan Courts case-opinion search API",
        &url,
        false,
    )
}

fn parse_michigan(json: &serde_json::Value) -> Vec<StateOpinion> {
    let Some(rows) = json.get("searchItems").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    rows.iter()
        .map(|row| {
            let s = |k: &str| {
                row.get(k)
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            };
            let b = |k: &str| row.get(k).and_then(|v| v.as_bool());
            let title = s("title").unwrap_or_default();
            let (case_name, disposition) = mi_split_title(title);
            // `courts[]` names the *originating* trial court ("LIVINGSTON
            // CIRCUIT COURT"), not the court that decided the appeal, so it is
            // the wrong value for this field. The document flags say who wrote
            // the opinion.
            let court = if b("isSupremeCourtDocument").unwrap_or(false) {
                "Michigan Supreme Court"
            } else if b("isCourtOfAppealsDocument").unwrap_or(false) {
                "Michigan Court of Appeals"
            } else {
                "Michigan"
            };
            let docket = first_str_of(row, "uniqueCourtOfAppealsCaseNumbers")
                .or_else(|| first_str_of(row, "supremeCourtCaseNumbers"))
                .or_else(|| {
                    if b("isSupremeCourtDocument").unwrap_or(false) {
                        s("mscCaseId").map(str::to_string)
                    } else {
                        s("coaCaseId").map(str::to_string)
                    }
                });
            StateOpinion {
                case_name: Some(clamp_text(&case_name, MAX_CASE_NAME).0),
                court: Some(court.to_string()),
                // "2026-06-24T04:00:00+00:00" — keep the date only; the time is
                // a midnight-Eastern artefact, not a filing time.
                filed: s("filingDate").and_then(|d| iso_prefix_date(d)),
                citation: None,
                docket,
                disposition,
                published: b("isPublished"),
                // documentUrl is a site-relative path; a caller handed a bare
                // "/4a4e60/siteassets/…" cannot fetch it.
                pdf_url: s("documentUrl").map(|u| absolute(MI_HOST, u)),
                url: s("caseUrl")
                    .map(|u| absolute(MI_HOST, u))
                    .or_else(|| s("documentUrl").map(|u| absolute(MI_HOST, u))),
            }
        })
        .collect()
}

/// "COA 335678 D ARTHUR CHAPMAN V OFFICER D MACK Opinion - Dissenting 06/19/2018"
/// -> ("ARTHUR CHAPMAN V OFFICER D MACK", "Opinion - Dissenting").
///
/// Michigan packs court, internal id, caption, disposition and date into one
/// string. Unsplit, the caption is unusable for matching and the disposition is
/// invisible; if the shape ever changes the whole title survives as the name.
fn mi_split_title(title: &str) -> (String, Option<String>) {
    static MI_TITLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?s)^(?:COA|MSC)\s+\d+\s+(?:[A-Z]\s+)?(.+?)\s+((?:Opinion|Order)\b.*?)(?:\s+\d{1,2}/\d{1,2}/\d{2,4})?$",
        )
        .expect("static regex")
    });
    match MI_TITLE.captures(title) {
        Some(c) => (
            c.get(1)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default(),
            c.get(2).map(|m| m.as_str().trim().to_string()),
        ),
        None => (title.trim().to_string(), None),
    }
}

fn first_str_of(row: &serde_json::Value, key: &str) -> Option<String> {
    row.get(key)?
        .as_array()?
        .iter()
        .find_map(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

// ── New Jersey ─────────────────────────────────────────────────────────────

async fn new_jersey(req: &StateOpinionsRequest) -> StateOpinionsResponse {
    let limit = effective_limit(req);
    let want = limit.saturating_mul(if req.unpublished_only { 2 } else { 1 });

    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut first_url = String::new();
    let mut offset = 0usize;
    for page in 0..NJ_MAX_PAGES {
        let url = nj_url(req, offset);
        if page == 0 {
            first_url = url.clone();
        }
        let (body, mut w) = get(&url, "njcourts jsonapi".to_string()).await;
        warnings.append(&mut w);
        let Some(text) = body else { break };
        let json: serde_json::Value = match serde_json::from_str(&text) {
            Ok(j) => j,
            Err(e) => {
                warnings.push(format!("njcourts jsonapi parse failed: {e}"));
                break;
            }
        };
        // JSON:API reports a 200 with an `errors` array for a bad filter or
        // sort field, which otherwise reads as an empty result set.
        if let Some(detail) = json
            .get("errors")
            .and_then(|e| e.as_array())
            .and_then(|a| a.first())
            .and_then(|e| e.get("detail"))
            .and_then(|d| d.as_str())
        {
            warnings.push(format!("njcourts jsonapi rejected the query: {detail}"));
            break;
        }
        let batch = parse_new_jersey(&json);
        let got = batch.len();
        items.extend(batch);
        if got < NJ_PAGE || items.len() >= want {
            break;
        }
        offset += NJ_PAGE;
    }

    // The trap that makes this API look dead when it is not: without
    // filter[status]=1 Drupal sorts unpublished *drafts* (editorial workflow
    // state, unrelated to precedential status) to the front and then strips
    // them for lack of authorization, so a page of 3 comes back holding 1 —
    // or 0, next to an "insufficient authorization" block. We always send it.
    if req.query.is_some() {
        warnings.push(
            "new jersey filtered server-side via filter[title][operator]=CONTAINS against all \
             ~23,900 published opinions (case-insensitive substring on the caption)."
                .to_string(),
        );
    }
    warnings.push(
        "njcourts opinion PDFs are served from /system/files/court-opinions/<year>/; older years \
         intermittently 403 while recent ones resolve."
            .to_string(),
    );

    finalize(
        "nj",
        req,
        items,
        warnings,
        "NJ Courts Drupal JSON:API (node/opinions)",
        &first_url,
        false,
    )
}

fn nj_url(req: &StateOpinionsRequest, offset: usize) -> String {
    let mut url = format!(
        "{NJ_HOST}/jsonapi/node/opinions?filter%5Bstatus%5D=1&sort=-field_posted_date\
         &page%5Blimit%5D={NJ_PAGE}&include=field_opinion.field_private_document,field_opinion_type"
    );
    if offset > 0 {
        url.push_str(&format!("&page%5Boffset%5D={offset}"));
    }
    if let Some(q) = req
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        url.push_str(&format!(
            "&filter%5Btitle%5D%5Boperator%5D=CONTAINS&filter%5Btitle%5D%5Bvalue%5D={}",
            urlencoding::encode(q)
        ));
    }
    url
}

/// JSON:API returns the related media, file and taxonomy records in a flat
/// `included` array keyed by (type, id); resolve them back onto each node.
fn parse_new_jersey(json: &serde_json::Value) -> Vec<StateOpinion> {
    let included: Vec<&serde_json::Value> = json
        .get("included")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let by_id = |id: &str| {
        included
            .iter()
            .find(|i| i.get("id").and_then(|v| v.as_str()) == Some(id))
    };
    let rel_id = |node: &serde_json::Value, name: &str| -> Option<String> {
        node.get("relationships")?
            .get(name)?
            .get("data")?
            .get("id")?
            .as_str()
            .map(str::to_string)
    };

    let Some(rows) = json.get("data").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    rows.iter()
        .map(|node| {
            let attrs = node.get("attributes").cloned().unwrap_or_default();
            let a = |k: &str| {
                attrs
                    .get(k)
                    .and_then(|v| v.as_str())
                    .filter(|v| !v.is_empty())
            };
            let title = a("title").unwrap_or_default();

            // The opinion-type taxonomy is the authoritative flag: its terms
            // are literally "Unpublished Appellate" / "Published Appellate" /
            // "Supreme" / "Unpublished Trial" / "Unpublished Tax".
            let term = rel_id(node, "field_opinion_type")
                .and_then(|id| by_id(&id).copied())
                .and_then(|t| t.get("attributes")?.get("name")?.as_str())
                .map(str::to_string);
            let (court, published) = match term.as_deref() {
                Some(t) => (
                    nj_court(t),
                    Some(!t.to_ascii_lowercase().starts_with("unpublished")),
                ),
                // Fall back to the "- Unpublished" title suffix, which is how
                // the site labels them for humans.
                None => (
                    "New Jersey".to_string(),
                    title
                        .to_ascii_lowercase()
                        .contains("unpublished")
                        .then_some(false),
                ),
            };

            // media--opinion.name is "A-1326-24 – CASE NAME (…)": the appellate
            // docket, which no attribute on the node itself carries.
            let media = rel_id(node, "field_opinion").and_then(|id| by_id(&id).copied());
            let docket = media
                .and_then(|m| m.get("attributes")?.get("name")?.as_str())
                .and_then(nj_docket)
                .or_else(|| a("field_opinion_id").map(str::to_string));
            let pdf = media
                .and_then(|m| rel_id(m, "field_private_document"))
                .and_then(|id| by_id(&id).copied())
                .and_then(|f| f.get("attributes")?.get("uri")?.get("url")?.as_str())
                .map(|u| absolute(NJ_HOST, u));

            let name = a("field_opinion_title")
                .map(str::to_string)
                .unwrap_or_else(|| nj_strip_status_suffix(title));
            StateOpinion {
                case_name: Some(clamp_text(&name, MAX_CASE_NAME).0),
                court: Some(court),
                filed: a("field_posted_date").and_then(iso_prefix_date),
                citation: None,
                docket,
                disposition: term,
                published,
                pdf_url: pdf,
                url: attrs
                    .get("path")
                    .and_then(|p| p.get("alias"))
                    .and_then(|v| v.as_str())
                    .map(|p| absolute(NJ_HOST, p)),
            }
        })
        .collect()
}

fn nj_court(term: &str) -> String {
    let t = term.to_ascii_lowercase();
    if t.contains("supreme") {
        "New Jersey Supreme Court".to_string()
    } else if t.contains("appellate") {
        "New Jersey Superior Court, Appellate Division".to_string()
    } else if t.contains("tax") {
        "New Jersey Tax Court".to_string()
    } else if t.contains("trial") {
        "New Jersey Superior Court (trial)".to_string()
    } else {
        format!("New Jersey ({term})")
    }
}

/// "A-1326-24 – CARLOS VERAS, ET AL. VS. …" -> "A-1326-24".
fn nj_docket(media_name: &str) -> Option<String> {
    let head = media_name.split(['\u{2013}', '\u{2014}']).next()?.trim();
    // Guard against a media record with no dash: the whole caption is not a
    // docket number.
    (!head.is_empty() && head.len() <= 32 && head.contains('-') && head != media_name.trim())
        .then(|| head.to_string())
}

fn nj_strip_status_suffix(title: &str) -> String {
    for suffix in [" - Unpublished", " - Published"] {
        if let Some(head) = title.strip_suffix(suffix) {
            return head.to_string();
        }
    }
    title.to_string()
}

// ── shared shaping ─────────────────────────────────────────────────────────

fn effective_limit(req: &StateOpinionsRequest) -> usize {
    if req.limit == 0 {
        DEFAULT_LIMIT
    } else {
        req.limit
    }
}

/// Apply the caller's filters, order, cap, and count the headline metric.
///
/// `client_side_query` distinguishes the two very different things `query` can
/// mean here, and says which one happened in `warnings`: for Michigan and New
/// Jersey the upstream searched its whole corpus; for the RSS feeds we filtered
/// the last few weeks of items ourselves.
fn finalize(
    state: &str,
    req: &StateOpinionsRequest,
    mut items: Vec<StateOpinion>,
    mut warnings: Vec<String>,
    source: &str,
    source_url: &str,
    client_side_query: bool,
) -> StateOpinionsResponse {
    if let Some(q) = req
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        if client_side_query {
            let needle = q.to_ascii_lowercase();
            let before = items.len();
            items.retain(|o| matches_query(o, &needle));
            warnings.push(format!(
                "{state}: no server-side search on this feed — {q:?} was matched client-side \
                 (substring, case-insensitive) over case name, disposition and docket across the \
                 {before} items in the current window."
            ));
        }
    }

    if req.unpublished_only {
        let before = items.len();
        items.retain(|o| o.published == Some(false));
        if before > 0 && items.is_empty() {
            warnings.push(
                "unpublished_only removed every item: none of the fetched opinions is marked \
                 non-precedential"
                    .to_string(),
            );
        }
    }

    // Multiple courts are merged for IL and PA, so impose a single order.
    // Undated items sort last rather than jumbling the top of the list.
    items.sort_by(|a, b| b.filed.cmp(&a.filed));

    let limit = effective_limit(req);
    if items.len() > limit {
        warnings.push(format!(
            "{} items matched; truncated to limit={limit}",
            items.len()
        ));
        items.truncate(limit);
    }

    StateOpinionsResponse {
        generated_at: Utc::now(),
        state: state.to_string(),
        returned: items.len(),
        unpublished_count: items.iter().filter(|o| o.published == Some(false)).count(),
        opinions: items,
        source: Some(source.to_string()),
        source_url: (!source_url.is_empty()).then(|| source_url.to_string()),
        warnings,
    }
}

fn matches_query(o: &StateOpinion, needle: &str) -> bool {
    [&o.case_name, &o.disposition, &o.docket, &o.citation]
        .into_iter()
        .flatten()
        .any(|f| f.to_ascii_lowercase().contains(needle))
}

// ── helpers ────────────────────────────────────────────────────────────────

/// One GET, returning the body and any degradation as warnings rather than an
/// error — so one dead feed never fails a multi-feed state.
async fn get(url: &str, source: String) -> (Option<String>, Vec<String>) {
    let mut warnings = Vec::new();
    let resp = match shared_client::GENERAL.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("{source} request failed: {e}"));
            return (None, warnings);
        }
    };
    let Some(resp) = soft_fail(&source, resp, &mut warnings).await else {
        return (None, warnings);
    };
    match resp.text().await {
        Ok(t) => (Some(t), warnings),
        Err(e) => {
            warnings.push(format!("{source} body read failed: {e}"));
            (None, warnings)
        }
    }
}

/// The inner slice of every `<item>…</item>`. There is no RSS crate in this
/// workspace and `scraper` mangles namespaced element names like
/// `<opinion:casename>`, so the feeds are sliced directly — which is also the
/// only thing that reads these two well-formed, machine-generated feeds
/// correctly without pulling in a parser.
fn rss_items(xml: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<item>") {
        let after = start + "<item>".len();
        let Some(end) = rest[after..].find("</item>") else {
            break;
        };
        out.push(&rest[after..after + end]);
        rest = &rest[after + end..];
    }
    out
}

/// Raw inner text of the first `<name>` element, attributes skipped.
fn tag_raw<'a>(item: &'a str, name: &str) -> Option<&'a str> {
    let start = item.find(&format!("<{name}"))?;
    // Step past the whole opening tag so `<guid isPermaLink="false">` does not
    // leak its attributes into the value.
    let after = start + item[start..].find('>')? + 1;
    let end = item[after..].find(&format!("</{name}>"))? + after;
    Some(&item[after..end])
}

fn tag_text(item: &str, name: &str) -> Option<String> {
    let text = clean(tag_raw(item, name)?);
    (!text.is_empty()).then_some(text)
}

/// Unwrap CDATA, drop markup, decode entities.
///
/// Illinois double-escapes its ampersands (`&amp;amp;` in the raw feed), so a
/// single decode pass leaves `&amp;` sitting in the case name. Run it twice
/// when the first pass still leaves an entity behind — bounded at two, so a
/// case name that legitimately contains "&amp;" as text can't loop.
fn clean(raw: &str) -> String {
    let raw = raw
        .trim()
        .trim_start_matches("<![CDATA[")
        .trim_end_matches("]]>");
    let once = strip_markup(raw);
    if once.contains("&amp;") || once.contains("&quot;") || once.contains("&#") {
        return strip_markup(&once);
    }
    once
}

/// "8/31/2026" -> "2026-08-31". Illinois' own `opinion:filingdate` format.
fn us_date(raw: &str) -> Option<String> {
    let mut parts = raw.trim().split('/');
    let m: u32 = parts.next()?.trim().parse().ok()?;
    let d: u32 = parts.next()?.trim().parse().ok()?;
    let y: i32 = parts.next()?.trim().parse().ok()?;
    iso(&format!("{y:04}-{m:02}-{d:02}"))
}

/// RSS `pubDate`: "Mon, 31 Aug 2026 04:00:00 GMT" -> "2026-08-31".
fn rfc2822_date(raw: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc2822(raw.trim())
        .ok()
        .map(|dt| dt.date_naive().to_string())
}

/// "2026-06-24T04:00:00+00:00" or "2026-06-24" -> "2026-06-24".
fn iso_prefix_date(raw: &str) -> Option<String> {
    iso(raw.trim().get(..10)?)
}

/// Everything that produces a `filed` value goes through here, so a malformed
/// upstream date becomes `None` rather than a string the caller has to guess at.
fn iso(candidate: &str) -> Option<String> {
    parse_date(candidate, "filed").ok().map(|d| d.to_string())
}

/// Both Michigan's `documentUrl` and New Jersey's `path.alias` are site-relative.
fn absolute(host: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    format!(
        "{host}{}{}",
        if path.starts_with('/') { "" } else { "/" },
        path
    )
}

/// Illinois' PDF hrefs contain literal spaces ("…/People v. Brown 2026 IL App
/// (1st) 230317-UB.pdf"), which is not a valid URL; a client that passes them
/// through unescaped gets a 400 rather than the opinion.
fn encode_spaces(url: &str) -> String {
    url.replace(' ', "%20")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(query: Option<&str>, unpublished_only: bool, limit: usize) -> StateOpinionsRequest {
        StateOpinionsRequest {
            state: "il".to_string(),
            query: query.map(str::to_string),
            unpublished_only,
            limit,
        }
    }

    /// Trimmed from the live feed, namespace declaration and all.
    const IL_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<rss version="2.0" xmlns:opinion="https://www.illinoiscourts.gov/top-level-opinions/">
<channel>
<item>
<guid isPermaLink="false">81819-opinion-www.illinoiscourts.gov</guid>
<category>Opinion</category>
<pubDate>Mon, 31 Aug 2026 00:00:00 GMT</pubDate>
<title>FNCB Bank &amp;amp; Trust v. MK Deliveries, Inc.</title>
<link>https://www.illinoiscourts.gov/resources/abc/FNCB Bank v. MK Deliveries.pdf</link>
<opinion:casename>FNCB Bank &amp;amp; Trust v. MK Deliveries, Inc.</opinion:casename>
<opinion:filingdate>8/31/2026</opinion:filingdate>
<opinion:type>Opinion</opinion:type>
<opinion:citationnum>2026 IL App (1st) 242281</opinion:citationnum>
<opinion:docketstatus>Slip</opinion:docketstatus>
<opinion:court>First District Appellate Court</opinion:court>
<opinion:notes></opinion:notes>
<opinion:pdf>https://www.illinoiscourts.gov/resources/abc/FNCB Bank v. MK Deliveries.pdf</opinion:pdf>
</item>
<item>
<guid isPermaLink="false">81821-opinion-www.illinoiscourts.gov</guid>
<category>Rule 23</category>
<pubDate>Mon, 31 Aug 2026 00:00:00 GMT</pubDate>
<title>People v. Brown</title>
<link>https://www.illinoiscourts.gov/resources/def/People v. Brown.pdf</link>
<opinion:casename>People v. Brown</opinion:casename>
<opinion:filingdate>8/28/2026</opinion:filingdate>
<opinion:type>Rule 23</opinion:type>
<opinion:citationnum>2026 IL App (1st) 230317-UB</opinion:citationnum>
<opinion:docketstatus>Slip</opinion:docketstatus>
<opinion:court>First District Appellate Court</opinion:court>
<opinion:notes></opinion:notes>
<opinion:pdf>https://www.illinoiscourts.gov/resources/def/People v. Brown.pdf</opinion:pdf>
</item>
</channel></rss>"#;

    #[test]
    fn illinois_rule_23_orders_come_back_unpublished() {
        let items = parse_illinois(IL_FIXTURE, "Appellate Court");
        assert_eq!(items.len(), 2, "both <item> elements parse");

        let rule23 = items
            .iter()
            .find(|o| o.case_name.as_deref() == Some("People v. Brown"))
            .expect("Rule 23 item present");
        assert_eq!(
            rule23.published,
            Some(false),
            "a Rule 23 order is non-precedential — this is the whole point of the module"
        );
        assert_eq!(rule23.disposition.as_deref(), Some("Rule 23 Order"));
        assert_eq!(rule23.filed.as_deref(), Some("2026-08-28"));
        assert_eq!(
            rule23.court.as_deref(),
            Some("First District Appellate Court")
        );

        let opinion = &items[0];
        assert_eq!(
            opinion.published,
            Some(true),
            "a published opinion is precedential"
        );
        // The feed double-escapes its ampersands; one decode pass is not enough.
        assert_eq!(
            opinion.case_name.as_deref(),
            Some("FNCB Bank & Trust v. MK Deliveries, Inc.")
        );
        // Literal spaces in the href are not a usable URL.
        assert_eq!(
            opinion.pdf_url.as_deref(),
            Some("https://www.illinoiscourts.gov/resources/abc/FNCB%20Bank%20v.%20MK%20Deliveries.pdf")
        );
    }

    #[test]
    fn illinois_citation_suffix_is_a_backstop_for_a_blank_type() {
        // If the type element ever goes empty, the "-U" citation still says it.
        assert_eq!(
            il_published("", Some("2026 IL App (3d) 250171-U")),
            Some(false)
        );
        assert_eq!(il_published("", Some("2026 IL App (1st) 242281")), None);
        assert_eq!(il_published("Opinion", Some("2026 IL 130000")), Some(true));
    }

    const PA_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/">
  <channel>
    <item>
      <title>Judgment List</title>
      <link>https://www.pacourts.us/assets/opinions/Superior/out/083126 E.pdf</link>
      <pubDate>Mon, 31 Aug 2026 04:00:00 GMT</pubDate>
      <description><![CDATA[]]></description>
      <dc:creator></dc:creator>
    </item>
    <item>
      <title>Com. v. Alvarez, A.
No. 2347 EDA 2025</title>
      <link>https://www.pacourts.us/assets/opinions/Superior/out/J-S25019-26m - 1069064.pdf</link>
      <pubDate>Mon, 31 Aug 2026 04:00:00 GMT</pubDate>
      <dc:creator>Beck, J.</dc:creator>
    </item>
    <item>
      <title>Com. v. Weimer, C.
No. 924 WDA 2025</title>
      <link>https://www.pacourts.us/assets/opinions/Superior/out/J-S06012-26o - 1069067.pdf</link>
      <pubDate>Fri, 28 Aug 2026 04:00:00 GMT</pubDate>
      <dc:creator>Stevens, P.J.E.</dc:creator>
    </item>
  </channel></rss>"#;

    #[test]
    fn pennsylvania_memoranda_are_unpublished_and_lists_are_dropped() {
        let (items, dropped) = parse_pennsylvania(PA_FIXTURE, "Superior Court");
        assert_eq!(dropped, 1, "the daily Judgment List is not an opinion");
        assert_eq!(items.len(), 2);

        let memo = &items[0];
        assert_eq!(memo.case_name.as_deref(), Some("Com. v. Alvarez, A."));
        assert_eq!(memo.docket.as_deref(), Some("2347 EDA 2025"));
        assert_eq!(memo.disposition.as_deref(), Some("Memorandum"));
        assert_eq!(
            memo.published,
            Some(false),
            "a …m filename is a Rule 65.37 memorandum — non-precedential"
        );
        assert_eq!(memo.filed.as_deref(), Some("2026-08-31"));

        assert_eq!(items[1].published, Some(true), "…o is a published opinion");
        assert_eq!(items[1].disposition.as_deref(), Some("Opinion"));
    }

    #[test]
    fn pennsylvania_title_shapes_all_yield_a_docket() {
        // Superior (newline, collapsed before matching), Commonwealth (" - "),
        // Supreme (" - No. ").
        assert_eq!(
            pa_split_title("Com. v. Alvarez, A. No. 2347 EDA 2025"),
            Some(("Com. v. Alvarez, A.".into(), "2347 EDA 2025".into()))
        );
        assert_eq!(
            pa_split_title("S.M. Connelly v. Keystone Home Health (WCAB) - 513 C.D. 2025"),
            Some((
                "S.M. Connelly v. Keystone Home Health (WCAB)".into(),
                "513 C.D. 2025".into()
            ))
        );
        assert_eq!(
            pa_split_title("Commonwealth v. Storms, M., Pet. - No. 125 MM 2026"),
            Some((
                "Commonwealth v. Storms, M., Pet.".into(),
                "125 MM 2026".into()
            ))
        );
        assert_eq!(pa_split_title("Judgment List"), None);
    }

    #[test]
    fn pennsylvania_disposition_codes() {
        assert_eq!(
            pa_superior_disposition("dm"),
            (Some("Dissenting Memorandum".into()), Some(false))
        );
        assert_eq!(
            pa_superior_disposition("pco"),
            (Some("Per Curiam Opinion".into()), Some(true))
        );
        // A judgment order is neither, so we say nothing rather than guess.
        assert_eq!(
            pa_superior_disposition("jo"),
            (Some("Judgment Order".into()), None)
        );
        assert_eq!(pa_superior_disposition(""), (None, None));
        // Commonwealth and Supreme filenames carry no precedential marker.
        assert_eq!(
            pa_disposition("Commonwealth Court", "513CD25_8-31-26.pdf"),
            (None, None)
        );
        assert_eq!(
            pa_disposition("Commonwealth Court", "1343CD24ORD_8-31-26.pdf"),
            (Some("Order".into()), None)
        );
    }

    const MI_FIXTURE: &str = r#"{
      "totalResults": 613, "pageSize": 2,
      "searchItems": [
        {
          "coaCaseId": "335678", "caseUrl": "/c/courts/coa/case/335678",
          "courts": ["WAYNE CIRCUIT COURT"],
          "filingDate": "2018-06-19T04:00:00+00:00",
          "documentUrl": "/4a4ff8/siteassets/case-documents/uploads/opinions/final/coa/335678d.opn.pdf",
          "isCourtOfAppealsCase": true, "isCourtOfAppealsDocument": true,
          "isFinalOpinion": false, "isPublished": false,
          "isSupremeCourtCase": false, "isSupremeCourtDocument": false,
          "mscCaseId": "158169", "supremeCourtCaseNumbers": [],
          "title": "COA 335678 D ARTHUR CHAPMAN V OFFICER D MACK Opinion - Dissenting 06/19/2018",
          "uniqueCourtOfAppealsCaseNumbers": []
        },
        {
          "coaCaseId": "229267", "caseUrl": "/c/courts/coa/case/229267",
          "courts": ["OAKLAND CIRCUIT COURT"],
          "filingDate": "2002-07-26T04:00:00+00:00",
          "documentUrl": "https://www.courts.michigan.gov/absolute/229267.pdf",
          "isCourtOfAppealsCase": true, "isCourtOfAppealsDocument": true,
          "isFinalOpinion": true, "isPublished": true,
          "isSupremeCourtCase": false, "isSupremeCourtDocument": false,
          "mscCaseId": "", "supremeCourtCaseNumbers": [],
          "title": "COA 229267 PEOPLE OF MI V CURTIS GRAYER SR Opinion - Per Curiam - Published 07/26/2002",
          "uniqueCourtOfAppealsCaseNumbers": ["229267"]
        }
      ]}"#;

    #[test]
    fn michigan_relative_document_url_becomes_absolute() {
        let json: serde_json::Value = serde_json::from_str(MI_FIXTURE).expect("fixture parses");
        let items = parse_michigan(&json);
        assert_eq!(items.len(), 2);

        assert_eq!(
            items[0].pdf_url.as_deref(),
            Some("https://www.courts.michigan.gov/4a4ff8/siteassets/case-documents/uploads/opinions/final/coa/335678d.opn.pdf"),
            "documentUrl is site-relative and unusable as returned"
        );
        // An already-absolute URL must not be prefixed twice.
        assert_eq!(
            items[1].pdf_url.as_deref(),
            Some("https://www.courts.michigan.gov/absolute/229267.pdf")
        );

        assert_eq!(items[0].published, Some(false));
        assert_eq!(items[1].published, Some(true));
        assert_eq!(
            items[0].case_name.as_deref(),
            Some("ARTHUR CHAPMAN V OFFICER D MACK")
        );
        assert_eq!(
            items[0].disposition.as_deref(),
            Some("Opinion - Dissenting")
        );
        assert_eq!(items[0].filed.as_deref(), Some("2018-06-19"));
        // The deciding court, not the "WAYNE CIRCUIT COURT" the case came from.
        assert_eq!(items[0].court.as_deref(), Some("Michigan Court of Appeals"));
        assert_eq!(items[1].docket.as_deref(), Some("229267"));
    }

    const NJ_FIXTURE: &str = r#"{
      "data": [
        {
          "type": "node--opinions", "id": "n1",
          "attributes": {
            "title": "Paramount Vending v. Kean - Unpublished",
            "field_opinion_title": "Paramount Vending v. Kean",
            "field_opinion_id": "MRS-L-324-18",
            "field_posted_date": "2023-06-26",
            "path": {"alias": "/court-opinion/paramount-vending-v-kean-unpublished"}
          },
          "relationships": {
            "field_opinion": {"data": {"type": "media--opinion", "id": "m1"}},
            "field_opinion_type": {"data": {"type": "taxonomy_term--opinions", "id": "t1"}}
          }
        },
        {
          "type": "node--opinions", "id": "n2",
          "attributes": {
            "title": "State v. Wyatt",
            "field_posted_date": "2026-08-28",
            "path": {"alias": "/court-opinion/state-v-wyatt"}
          },
          "relationships": {
            "field_opinion": {"data": {"type": "media--opinion", "id": "m2"}},
            "field_opinion_type": {"data": {"type": "taxonomy_term--opinions", "id": "t2"}}
          }
        }
      ],
      "included": [
        {"type": "media--opinion", "id": "m1",
         "attributes": {"name": "A-1326-24 – PARAMOUNT VENDING VS. KEAN (L-324-18)"},
         "relationships": {"field_private_document": {"data": {"type": "file--file", "id": "f1"}}}},
        {"type": "media--opinion", "id": "m2",
         "attributes": {"name": "A-0432-23 – STATE OF NEW JERSEY VS. ALVIN J. WYATT"},
         "relationships": {"field_private_document": {"data": {"type": "file--file", "id": "f2"}}}},
        {"type": "file--file", "id": "f1",
         "attributes": {"uri": {"url": "/system/files/court-opinions/2023/a1326-24.pdf"}}},
        {"type": "file--file", "id": "f2",
         "attributes": {"uri": {"url": "/system/files/court-opinions/2026/a0432-23.pdf"}}},
        {"type": "taxonomy_term--opinions", "id": "t1", "attributes": {"name": "Unpublished Appellate"}},
        {"type": "taxonomy_term--opinions", "id": "t2", "attributes": {"name": "Published Appellate"}}
      ]}"#;

    #[test]
    fn new_jersey_taxonomy_drives_the_published_flag() {
        let json: serde_json::Value = serde_json::from_str(NJ_FIXTURE).expect("fixture parses");
        let items = parse_new_jersey(&json);
        assert_eq!(items.len(), 2);

        assert_eq!(items[0].published, Some(false), "\"Unpublished Appellate\"");
        assert_eq!(
            items[0].disposition.as_deref(),
            Some("Unpublished Appellate")
        );
        assert_eq!(
            items[0].court.as_deref(),
            Some("New Jersey Superior Court, Appellate Division")
        );
        assert_eq!(
            items[0].case_name.as_deref(),
            Some("Paramount Vending v. Kean")
        );
        // Docket lives only on the related media record's name.
        assert_eq!(items[0].docket.as_deref(), Some("A-1326-24"));
        assert_eq!(
            items[0].pdf_url.as_deref(),
            Some("https://www.njcourts.gov/system/files/court-opinions/2023/a1326-24.pdf")
        );
        assert_eq!(
            items[0].url.as_deref(),
            Some("https://www.njcourts.gov/court-opinion/paramount-vending-v-kean-unpublished")
        );
        assert_eq!(items[1].published, Some(true), "\"Published Appellate\"");
    }

    #[test]
    fn new_jersey_url_always_carries_the_mandatory_status_filter() {
        // Without filter[status]=1 Drupal sorts unpublished editorial drafts
        // first, strips them for lack of authorization, and returns a short or
        // empty page that reads like a dead API. Verified live.
        let url = nj_url(&req(Some("motor vehicle"), false, 10), 0);
        assert!(url.contains("filter%5Bstatus%5D=1"), "{url}");
        assert!(url.contains("sort=-field_posted_date"), "{url}");
        assert!(
            url.contains("filter%5Btitle%5D%5Boperator%5D=CONTAINS"),
            "{url}"
        );
        assert!(url.contains("motor%20vehicle"), "{url}");
        let paged = nj_url(&req(None, false, 10), 50);
        assert!(paged.contains("page%5Boffset%5D=50"), "{paged}");
        assert!(
            !paged.contains("filter%5Btitle%5D"),
            "no query, no title filter"
        );
    }

    #[test]
    fn unpublished_only_keeps_exactly_the_non_precedential_items() {
        let items = parse_illinois(IL_FIXTURE, "Appellate Court");
        let out = finalize(
            "il",
            &req(None, true, 50),
            items,
            Vec::new(),
            "test",
            "u",
            true,
        );
        assert_eq!(out.returned, 1);
        assert_eq!(out.unpublished_count, 1);
        assert_eq!(
            out.opinions[0].case_name.as_deref(),
            Some("People v. Brown")
        );

        // Items whose status the source does not state are NOT unpublished.
        let unknown = vec![StateOpinion {
            case_name: Some("Unknown status".into()),
            published: None,
            ..Default::default()
        }];
        let out = finalize(
            "pa",
            &req(None, true, 50),
            unknown,
            Vec::new(),
            "test",
            "u",
            true,
        );
        assert_eq!(out.returned, 0);
    }

    #[test]
    fn client_side_query_filters_and_says_so() {
        let items = parse_illinois(IL_FIXTURE, "Appellate Court");
        let out = finalize(
            "il",
            &req(Some("brown"), false, 50),
            items,
            Vec::new(),
            "t",
            "u",
            true,
        );
        assert_eq!(out.returned, 1, "case-insensitive substring on the caption");
        assert_eq!(out.unpublished_count, 1);
        assert!(
            out.warnings.iter().any(|w| w.contains("client-side")),
            "the caller must know this was not a server-side search: {:?}",
            out.warnings
        );

        // Disposition is searchable too — "rule 23" is how a user asks for them.
        let items = parse_illinois(IL_FIXTURE, "Appellate Court");
        let out = finalize(
            "il",
            &req(Some("Rule 23"), false, 50),
            items,
            Vec::new(),
            "t",
            "u",
            true,
        );
        assert_eq!(out.returned, 1);

        // A server-side state must not re-filter, and must not claim it did.
        let items = parse_illinois(IL_FIXTURE, "Appellate Court");
        let out = finalize(
            "mi",
            &req(Some("nomatch"), false, 50),
            items,
            Vec::new(),
            "t",
            "u",
            false,
        );
        assert_eq!(out.returned, 2);
        assert!(!out.warnings.iter().any(|w| w.contains("client-side")));
    }

    #[test]
    fn results_are_newest_first_and_capped_at_the_limit() {
        let items = parse_illinois(IL_FIXTURE, "Appellate Court");
        let out = finalize(
            "il",
            &req(None, false, 1),
            items,
            Vec::new(),
            "t",
            "u",
            true,
        );
        assert_eq!(out.returned, 1);
        assert_eq!(out.opinions[0].filed.as_deref(), Some("2026-08-31"));
        assert!(out
            .warnings
            .iter()
            .any(|w| w.contains("truncated to limit=1")));
    }

    #[test]
    fn a_zero_limit_is_treated_as_unset_rather_than_as_an_empty_answer() {
        let items = parse_illinois(IL_FIXTURE, "Appellate Court");
        let out = finalize(
            "il",
            &req(None, false, 0),
            items,
            Vec::new(),
            "t",
            "u",
            true,
        );
        assert_eq!(out.returned, 2);
    }

    #[test]
    fn malformed_dates_degrade_to_none_instead_of_a_bogus_string() {
        assert_eq!(us_date("8/31/2026").as_deref(), Some("2026-08-31"));
        assert_eq!(us_date("31/8/2026"), None, "month 31 does not exist");
        assert_eq!(us_date("not a date"), None);
        assert_eq!(
            iso_prefix_date("2026-06-24T04:00:00+00:00").as_deref(),
            Some("2026-06-24")
        );
        assert_eq!(iso_prefix_date("06/24/2026"), None);
        assert_eq!(
            rfc2822_date("Mon, 31 Aug 2026 04:00:00 GMT").as_deref(),
            Some("2026-08-31")
        );
    }

    #[test]
    fn rss_slicing_survives_a_truncated_feed() {
        assert_eq!(rss_items("<item>a</item><item>b</item>"), vec!["a", "b"]);
        // A feed cut off mid-item yields what completed, not a panic.
        assert_eq!(rss_items("<item>a</item><item>truncated"), vec!["a"]);
        assert!(rss_items("<channel></channel>").is_empty());
    }

    #[test]
    fn nj_docket_refuses_to_invent_one() {
        assert_eq!(
            nj_docket("A-1326-24 \u{2013} SOME CASE").as_deref(),
            Some("A-1326-24")
        );
        // No dash separator: the whole caption is not a docket number.
        assert_eq!(nj_docket("SOME CASE WITH NO DOCKET"), None);
    }
}
