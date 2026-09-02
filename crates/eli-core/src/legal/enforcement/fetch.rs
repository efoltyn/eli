//! Agency enforcement actions — SEC, DOJ, and what CFPB actually publishes.
//!
//! Why this exists when every one of these is a press release on a public
//! website: none of the three lets you *ask a question across them*. SEC's
//! litigation-release index is a Drupal table whose only filters are year and
//! month (`?search=` is silently ignored); DOJ's API accepts a title filter but
//! ignores `sort=` entirely and hands back 271,000 press releases oldest-first,
//! so "the newest antitrust case" is page 13 of 14 and there is no way to ask
//! for it directly; CFPB's enforcement page returns HTML no matter what
//! `format` you pass. This module does the paging, the reordering and the
//! date/keyword filtering that the upstreams won't, and returns one list across
//! all three sorted newest-first.
//!
//! **Honesty rules baked in.** A per-source failure is a `warnings` line, never
//! a failed call — a SEC outage must not cost you the DOJ results. And what
//! CFPB exposes key-free is its *consumer complaint database*, which is
//! allegations by members of the public, not findings by the agency; those rows
//! are tagged `cfpb-complaints`, not `cfpb`, and carry a warning saying so.
//!
//! **SEC requires a contact email in the User-Agent.** Every `*.sec.gov` host
//! answers 403 with "Your Request Originates from an Undeclared Automated Tool"
//! unless the UA contains one. That is a content check on the header, not a
//! rate limit, so no amount of backing off fixes it. This module reads the
//! address from the environment (see [`sec_user_agent`]) rather than shipping
//! one, and when it is missing it says so and *still sends the request*, so the
//! 403 is visible instead of silently swallowed.
//!
//! No API keys anywhere in this module.

use crate::legal::{clamp_text, parse_date, shared_client, soft_fail, strip_markup};
use crate::{Error, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

const SEC_LITIGATION_RSS: &str = "https://www.sec.gov/enforcement-litigation/litigation-releases/rss";
const SEC_ADMIN_RSS: &str =
    "https://www.sec.gov/enforcement-litigation/administrative-proceedings/rss";
const DOJ_BASE: &str = "https://www.justice.gov/api/v1/press_releases.json";
const CFPB_BASE: &str =
    "https://www.consumerfinance.gov/data-research/consumer-complaints/search/api/v1/";

/// Both SEC feeds are "latest 25", with no date or keyword parameters —
/// documented here because it bounds what `--after` and `--q` can possibly
/// return from SEC.
const SEC_FEED_DEPTH: usize = 25;

/// DOJ press-release bodies are full HTML articles; keep a readable lead.
const SUMMARY_CAP: usize = 700;

#[derive(Clone, Debug)]
pub struct EnforcementRequest {
    /// `sec` | `cfpb` | `doj` | `all`, or a comma list of those.
    pub source: String,
    pub query: Option<String>,
    pub after: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct EnforcementAction {
    /// `sec-litigation` | `sec-admin` | `doj` | `cfpb-complaints`. Specific
    /// rather than just the agency, because an administrative proceeding, a
    /// federal-court complaint and a consumer complaint are three different
    /// kinds of claim and shouldn't be conflated in a single bucket.
    pub source: String,
    pub title: Option<String>,
    /// `YYYY-MM-DD`, normalized across three very different upstream formats so
    /// the merged list can be sorted lexically.
    pub date: Option<String>,
    /// SEC release number ("LR-26623", "34-106227"), DOJ release number, or the
    /// CFPB complaint id.
    pub release_number: Option<String>,
    /// Only populated when the party is stated unambiguously — a caption
    /// ("SEC v. X", "In the Matter of Y"), a corporate-suffixed release title,
    /// or a structured company field. Never inferred from prose.
    pub respondents: Vec<String>,
    pub summary: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnforcementResponse {
    pub generated_at: DateTime<Utc>,
    /// The sources actually queried, after expanding `all`.
    pub sources: Vec<String>,
    pub actions: Vec<EnforcementAction>,
    pub warnings: Vec<String>,
}

pub async fn fetch_enforcement(req: EnforcementRequest) -> Result<EnforcementResponse> {
    let mut warnings: Vec<String> = Vec::new();
    let sources = normalize_sources(&req.source, &mut warnings)?;
    // Validate before spending requests, and keep the parsed form for filtering.
    let after = match req.after.as_deref() {
        Some(d) => Some(parse_date(d, "--after")?.to_string()),
        None => None,
    };
    let limit = req.limit.max(1);

    // Independent upstreams on three different hosts — run them together. A
    // serial walk here costs several seconds for no reason, and one slow host
    // would gate the others.
    let want = |s: &str| sources.iter().any(|x| x == s);
    let (sec, doj, cfpb) = futures::join!(
        async {
            if want("sec") {
                fetch_sec(req.query.as_deref(), after.as_deref(), limit).await
            } else {
                (Vec::new(), Vec::new())
            }
        },
        async {
            if want("doj") {
                fetch_doj(req.query.as_deref(), after.as_deref(), limit).await
            } else {
                (Vec::new(), Vec::new())
            }
        },
        async {
            if want("cfpb") {
                fetch_cfpb(req.query.as_deref(), after.as_deref(), limit).await
            } else {
                (Vec::new(), Vec::new())
            }
        },
    );

    let mut actions = Vec::new();
    for (rows, warns) in [sec, doj, cfpb] {
        actions.extend(rows);
        warnings.extend(warns);
    }

    // Newest first across the merged list. `None` dates sort last: `Option`
    // orders `None` below `Some`, and this comparison is reversed.
    actions.sort_by(|a, b| b.date.cmp(&a.date));

    if actions.is_empty() {
        warnings.push(
            "no actions matched. --q filters SEC by keyword client-side over a 25-item feed and \
             DOJ by a server-side title substring, so a term that appears only in a release body \
             will miss."
                .to_string(),
        );
    }

    Ok(EnforcementResponse {
        generated_at: Utc::now(),
        sources,
        actions,
        warnings,
    })
}

/// Expand `all`, accept comma lists, and warn (rather than fail) on a name we
/// don't serve — a typo shouldn't cost the caller the sources that did resolve.
fn normalize_sources(raw: &str, warnings: &mut Vec<String>) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str, out: &mut Vec<String>| {
        if !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };
    for token in raw.split(',').map(|t| t.trim().to_lowercase()) {
        match token.as_str() {
            "" => {}
            "all" => {
                push("sec", &mut out);
                push("doj", &mut out);
                push("cfpb", &mut out);
            }
            "sec" | "doj" | "cfpb" => push(&token, &mut out),
            "ftc" => warnings.push(
                "ftc: skipped. www.ftc.gov serves an Akamai 403 to every non-browser User-Agent, \
                 including its own RSS feed, and has no JSON API. Spoofing a browser UA to get \
                 around that is not something this tool does."
                    .to_string(),
            ),
            other => warnings.push(format!(
                "unknown source {other:?} — supported: sec, doj, cfpb, all"
            )),
        }
    }
    if out.is_empty() {
        return Err(Error::InvalidInput(format!(
            "--source {raw:?} selected no usable source; use sec, doj, cfpb or all"
        )));
    }
    Ok(out)
}

// ── SEC ────────────────────────────────────────────────────────────────────

/// Resolve a User-Agent SEC will accept, i.e. one containing a contact email.
///
/// Checked in order: `SEC_USER_AGENT`, `ELI_SEC_USER_AGENT` (the variable the
/// existing `market-search config --set sec_user_agent` path already uses for
/// EDGAR), the saved `chat.sec_user_agent` config value, then
/// `LEGAL_SEARCH_USER_AGENT`. Returns `None` when none of them carries an
/// address — the caller then warns and sends the default anyway, so the 403 is
/// reported rather than hidden behind a "no results" answer.
fn sec_user_agent() -> Option<String> {
    for var in ["SEC_USER_AGENT", "ELI_SEC_USER_AGENT", "LEGAL_SEARCH_USER_AGENT"] {
        if let Ok(v) = std::env::var(var) {
            if looks_like_contact_ua(&v) {
                return Some(v.trim().to_string());
            }
        }
    }
    // Read once: this is a synchronous file read and the answer never changes
    // within a process.
    static CONFIGURED: LazyLock<Option<String>> = LazyLock::new(|| {
        let paths = crate::config::Paths::discover().ok()?;
        let cfg = crate::config::load_or_default(&paths).ok()?;
        cfg.chat.sec_user_agent.filter(|v| looks_like_contact_ua(v))
    });
    CONFIGURED.clone()
}

/// SEC's documented format is "Sample Company Name AdminContact@domain.com" —
/// what it actually checks for is an address-shaped token.
fn looks_like_contact_ua(ua: &str) -> bool {
    let ua = ua.trim();
    let Some((local, rest)) = ua.split_once('@') else {
        return false;
    };
    let domain: String = rest
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != ')' && *c != '>')
        .collect();
    !local.trim().is_empty() && domain.contains('.') && !domain.ends_with('.')
}

const SEC_UA_WARNING: &str =
    "sec: no contact email in the User-Agent. Every *.sec.gov host answers 403 \
     (\"Your Request Originates from an Undeclared Automated Tool\") unless the UA contains one — \
     it is a check on the header's content, not a rate limit. Set \
     SEC_USER_AGENT=\"your-org (you@example.com)\" (ELI_SEC_USER_AGENT and \
     `market-search config --set sec_user_agent --value \"...\"` are also read). Requesting anyway \
     so the failure is visible.";

async fn fetch_sec(
    query: Option<&str>,
    after: Option<&str>,
    limit: usize,
) -> (Vec<EnforcementAction>, Vec<String>) {
    let mut warnings = Vec::new();
    let ua = match sec_user_agent() {
        Some(ua) => ua,
        None => {
            warnings.push(SEC_UA_WARNING.to_string());
            shared_client::user_agent()
        }
    };

    // Two feeds, two kinds of proceeding: federal-court litigation releases and
    // in-house administrative orders. Fetched together.
    let (lit, admin) = futures::join!(
        sec_feed(SEC_LITIGATION_RSS, "sec-litigation", &ua),
        sec_feed(SEC_ADMIN_RSS, "sec-admin", &ua),
    );

    let mut actions = Vec::new();
    for (rows, warns) in [lit, admin] {
        actions.extend(rows);
        warnings.extend(warns);
    }

    let before = actions.len();
    actions.retain(|a| matches_query(a, query) && after_ok(a, after));
    actions.sort_by(|a, b| b.date.cmp(&a.date));
    actions.truncate(limit);

    // The feeds are a fixed "latest 25" window with no date or search
    // parameters, so a filter that empties them is a coverage limit, not an
    // absence of matching cases. Say which.
    if before >= SEC_FEED_DEPTH && actions.is_empty() && (query.is_some() || after.is_some()) {
        warnings.push(format!(
            "sec: the RSS feeds expose only the latest {SEC_FEED_DEPTH} releases each and accept \
             no date or keyword parameters (their year/month query params are ignored), so this \
             filter found nothing within that window. For older releases browse \
             https://www.sec.gov/enforcement-litigation/litigation-releases?year=YYYY&month=M"
        ));
    }
    (actions, warnings)
}

async fn sec_feed(url: &str, source: &str, ua: &str) -> (Vec<EnforcementAction>, Vec<String>) {
    let mut warnings = Vec::new();
    // Per-request UA override rather than a second client: the shared client's
    // UA is fine for every other legal host, and SEC is the only one that cares.
    let resp = match shared_client::GENERAL
        .get(url)
        .header(reqwest::header::USER_AGENT, ua)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("{source} request failed: {e}"));
            return (Vec::new(), warnings);
        }
    };
    let Some(resp) = soft_fail(source, resp, &mut warnings).await else {
        return (Vec::new(), warnings);
    };
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            warnings.push(format!("{source} body read failed: {e}"));
            return (Vec::new(), warnings);
        }
    };

    let items = parse_rss_items(&body);
    if items.is_empty() {
        warnings.push(format!("{source}: feed parsed to zero items"));
    }
    let actions = items
        .into_iter()
        .map(|it| EnforcementAction {
            source: source.to_string(),
            // SEC's RSS <title> *is* the case caption — the release title
            // field holds the party names, nothing else — so a bare title may
            // be read as a party here. DOJ's headlines are sentences and get
            // no such licence.
            respondents: extract_respondents(&it.title, true),
            title: (!it.title.is_empty()).then(|| it.title.clone()),
            date: it.date,
            // SEC puts the release number in <dc:creator>, which is the one
            // field that says whether this is LR-26623 or 34-106227.
            release_number: it.creator,
            // The RSS description repeats the title verbatim on both feeds, so
            // it carries no information; better an empty summary than a
            // duplicated one that looks like substance.
            summary: it.description.filter(|d| d != &it.title),
            url: it.link,
        })
        .collect();
    (actions, warnings)
}

// ── DOJ ────────────────────────────────────────────────────────────────────

/// DOJ's Drupal API ignores `sort=`/`direction=` and always returns
/// oldest-first, so "recent" means jumping to the *last* page. We read the
/// result count from a one-row probe, compute the tail, and pull it.
async fn fetch_doj(
    query: Option<&str>,
    after: Option<&str>,
    limit: usize,
) -> (Vec<EnforcementAction>, Vec<String>) {
    let mut warnings = Vec::new();
    let title_filter = query
        .map(|q| format!("&title={}", urlencoding::encode(q)))
        .unwrap_or_default();

    let probe_url = format!("{DOJ_BASE}?pagesize=1&page=0{title_filter}");
    let Some(probe) = doj_get(&probe_url, &mut warnings).await else {
        return (Vec::new(), warnings);
    };
    let count = probe
        .get("metadata")
        .and_then(|m| m.get("resultset"))
        .and_then(|r| r.get("count"))
        .and_then(|c| c.as_str().and_then(|s| s.parse::<usize>().ok()).or_else(|| c.as_u64().map(|n| n as usize)))
        .unwrap_or(0);
    if count == 0 {
        warnings.push(format!(
            "doj: no press releases with {:?} in the title. `title=` is a substring match on the \
             headline only — the body is not searched.",
            query.unwrap_or("")
        ));
        return (Vec::new(), warnings);
    }

    let page_size = limit.clamp(10, 50);
    let last_page = count.div_ceil(page_size).saturating_sub(1);
    // Two tail pages cover `limit` even after the `--after` filter drops rows;
    // more than that is bandwidth spent on releases we will discard.
    let pages: Vec<usize> = (0..2)
        .filter_map(|i| last_page.checked_sub(i))
        .collect::<Vec<_>>();

    let fetched = futures::future::join_all(pages.iter().map(|p| {
        let url = format!("{DOJ_BASE}?pagesize={page_size}&page={p}{title_filter}");
        async move {
            let mut w = Vec::new();
            let v = doj_get(&url, &mut w).await;
            (v, w)
        }
    }))
    .await;

    let mut actions: Vec<EnforcementAction> = Vec::new();
    for (value, warns) in fetched {
        warnings.extend(warns);
        let Some(v) = value else { continue };
        if let Some(arr) = v.get("results").and_then(|r| r.as_array()) {
            for row in arr {
                if let Some(a) = doj_to_action(row) {
                    actions.push(a);
                }
            }
        }
    }

    actions.retain(|a| after_ok(a, after));
    // The upstream order is oldest-first *within* a page as well as across
    // pages, so this reordering is the whole point of the module for DOJ.
    actions.sort_by(|a, b| b.date.cmp(&a.date));
    actions.dedup_by(|a, b| a.url.is_some() && a.url == b.url);
    actions.truncate(limit);

    if count > page_size * 2 {
        warnings.push(format!(
            "doj: {count} releases match; returned the most recent from the last {} of \
             {} pages (the API ignores sort= and pages oldest-first).",
            pages.len(),
            last_page + 1
        ));
    }
    (actions, warnings)
}

fn doj_to_action(row: &serde_json::Value) -> Option<EnforcementAction> {
    let title = row
        .get("title")
        .and_then(|t| t.as_str())
        .map(|t| strip_markup(t))?;
    // `date` is a Unix epoch in a JSON *string*; `created`/`changed` wrap the
    // same number in an HTML <time> element.
    let date = row
        .get("date")
        .and_then(|d| d.as_str())
        .and_then(epoch_to_day);
    let summary = row
        .get("teaser")
        .and_then(|t| t.as_str())
        .filter(|t| !t.trim().is_empty())
        .or_else(|| row.get("body").and_then(|b| b.as_str()))
        .map(|s| clamp_text(&strip_markup(s), SUMMARY_CAP).0)
        .filter(|s| !s.is_empty());

    Some(EnforcementAction {
        source: "doj".to_string(),
        // `false`: a DOJ headline is prose. "Statement ... of the Merger of
        // Seismic Software Inc. and Highspot Inc." ends in a corporate suffix
        // and names no defendant; only an explicit "United States v. X"
        // caption counts.
        respondents: extract_respondents(&title, false),
        title: Some(title),
        date,
        release_number: row
            .get("number")
            .and_then(|n| n.as_str())
            .map(str::to_string)
            .filter(|n| !n.trim().is_empty()),
        summary,
        url: row.get("url").and_then(|u| u.as_str()).map(str::to_string),
    })
}

async fn doj_get(url: &str, warnings: &mut Vec<String>) -> Option<serde_json::Value> {
    get_json(url, "doj", warnings).await
}

// ── CFPB ───────────────────────────────────────────────────────────────────

/// What CFPB publishes key-free is complaints, not enforcement.
///
/// `consumerfinance.gov/enforcement/actions/?format=json` ignores the parameter
/// and returns HTML, and the enforcement RSS feed at `/enforcement/actions/feed/`
/// currently serves a 115-byte truncated document with zero items. The complaint
/// database is a real, queryable, key-free corpus — it is just a different
/// thing, and is labelled as such.
async fn fetch_cfpb(
    query: Option<&str>,
    after: Option<&str>,
    limit: usize,
) -> (Vec<EnforcementAction>, Vec<String>) {
    let mut warnings = Vec::new();
    // `no_aggs=true` drops the facet block: 92 KB -> 7 KB for the same rows.
    let mut url = format!(
        "{CFPB_BASE}?size={}&no_aggs=true&sort=created_date_desc",
        limit.clamp(1, 100)
    );
    if let Some(q) = query {
        url.push_str(&format!("&search_term={}", urlencoding::encode(q)));
    }
    if let Some(d) = after {
        url.push_str(&format!("&date_received_min={d}"));
    }

    let Some(v) = get_json(&url, "cfpb", &mut warnings).await else {
        return (Vec::new(), warnings);
    };

    let mut actions = Vec::new();
    if let Some(hits) = v
        .get("hits")
        .and_then(|h| h.get("hits"))
        .and_then(|h| h.as_array())
    {
        for hit in hits {
            if let Some(a) = cfpb_to_action(hit) {
                actions.push(a);
            }
        }
    }
    let total = v
        .get("hits")
        .and_then(|h| h.get("total"))
        .and_then(|t| t.get("value"))
        .and_then(|t| t.as_u64());

    if actions.is_empty() {
        warnings.push("cfpb: no complaints matched".to_string());
    } else {
        warnings.push(format!(
            "cfpb: these {} rows are consumer complaints from the CFPB complaint database{}, not \
             enforcement actions — they are unverified allegations by members of the public, and \
             `respondents` is the company the complaint names, not a party CFPB has charged. \
             CFPB publishes no machine-readable list of its own enforcement actions \
             (/enforcement/actions/?format=json returns HTML; the RSS feed is empty).",
            actions.len(),
            total.map(|t| format!(" ({t} matching)")).unwrap_or_default()
        ));
    }
    (actions, warnings)
}

fn cfpb_to_action(hit: &serde_json::Value) -> Option<EnforcementAction> {
    let src = hit.get("_source")?;
    let s = |k: &str| {
        src.get(k)
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .filter(|x| !x.trim().is_empty())
    };
    let company = s("company");
    let issue = s("issue");
    let product = s("product");

    // A headline the caller can read without opening the row.
    let title = match (&company, &issue, &product) {
        (Some(c), Some(i), _) => format!("{c} — {i}"),
        (Some(c), None, Some(p)) => format!("{c} — {p}"),
        (Some(c), None, None) => c.clone(),
        (None, Some(i), _) => i.clone(),
        _ => return None,
    };

    let id = hit
        .get("_id")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .or_else(|| s("complaint_id"));

    Some(EnforcementAction {
        source: "cfpb-complaints".to_string(),
        title: Some(title),
        date: s("date_received").map(|d| iso_day(&d)),
        url: id.as_ref().map(|i| {
            format!("https://www.consumerfinance.gov/data-research/consumer-complaints/search/detail/{i}")
        }),
        release_number: id,
        // The named company is a structured field, not something parsed out of
        // prose — the one place in this module where a respondent is certain.
        respondents: company.into_iter().collect(),
        summary: s("complaint_what_happened")
            .map(|t| clamp_text(&strip_markup(&t), SUMMARY_CAP).0)
            .or_else(|| s("company_response")),
    })
}

// ── shared helpers ─────────────────────────────────────────────────────────

async fn get_json(
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
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            warnings.push(format!("{source} body read failed: {e}"));
            return None;
        }
    };
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(v) => Some(v),
        Err(e) => {
            // Several of these hosts answer 200-with-HTML when a route is gone
            // rather than 404, so say what actually arrived.
            let head: String = body.chars().take(80).collect();
            warnings.push(format!("{source} parse failed ({e}); body began {head:?}"));
            None
        }
    }
}

#[derive(Debug, Default, Clone)]
struct RssItem {
    title: String,
    link: Option<String>,
    description: Option<String>,
    date: Option<String>,
    creator: Option<String>,
}

static ITEM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<item[^>]*>(.*?)</item>").expect("static item regex"));

/// Minimal RSS 2.0 item reader. There is no RSS crate in this workspace, and
/// pulling one in for two feeds with a fixed five-field shape is not worth a
/// dependency — but note this is deliberately *not* a general XML parser: it
/// reads the tags these two feeds emit and ignores everything else.
fn parse_rss_items(xml: &str) -> Vec<RssItem> {
    ITEM_RE
        .captures_iter(xml)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .map(|block| RssItem {
            title: rss_tag(block, "title").unwrap_or_default(),
            // SEC's feed puts a newline before </link>; every consumer of this
            // URL would otherwise carry it.
            link: rss_tag(block, "link").filter(|s| !s.is_empty()),
            description: rss_tag(block, "description").filter(|s| !s.is_empty()),
            date: rss_tag(block, "pubDate").as_deref().and_then(rfc2822_to_day),
            creator: rss_tag(block, "dc:creator").filter(|s| !s.is_empty()),
        })
        .collect()
}

/// Pull one element's text, unwrapping CDATA and flattening any inline markup.
fn rss_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let rest = &block[start..];
    let end = rest.find(&close)?;
    let raw = &rest[..end];
    let raw = raw
        .trim()
        .strip_prefix("<![CDATA[")
        .and_then(|r| r.strip_suffix("]]>"))
        .unwrap_or(raw);
    Some(strip_markup(raw))
}

/// RSS `pubDate` is RFC 2822 ("Fri, 28 Aug 2026 17:49:59 -0400"). Normalize to
/// a day so SEC rows sort against DOJ epochs and CFPB ISO timestamps.
fn rfc2822_to_day(s: &str) -> Option<String> {
    DateTime::parse_from_rfc2822(s.trim())
        .ok()
        .map(|d| d.date_naive().to_string())
}

/// DOJ's `date` is a Unix epoch carried as a string.
fn epoch_to_day(s: &str) -> Option<String> {
    let secs: i64 = s.trim().parse().ok()?;
    DateTime::from_timestamp(secs, 0).map(|d| d.date_naive().to_string())
}

/// CFPB's `date_received` is `2026-07-29T19:06:31.000Z`.
fn iso_day(ts: &str) -> String {
    ts.split('T').next().unwrap_or(ts).to_string()
}

/// Client-side keyword filter, for upstreams with no search parameter of their
/// own (the SEC feeds). Case-insensitive substring over title plus summary.
fn matches_query(a: &EnforcementAction, query: Option<&str>) -> bool {
    let Some(q) = query.map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty()) else {
        return true;
    };
    let hay = format!(
        "{} {} {}",
        a.title.clone().unwrap_or_default(),
        a.summary.clone().unwrap_or_default(),
        a.respondents.join(" ")
    )
    .to_lowercase();
    hay.contains(&q)
}

/// `--after` is inclusive. A row with no date is kept — dropping it would
/// silently hide an action because its upstream omitted a field.
fn after_ok(a: &EnforcementAction, after: Option<&str>) -> bool {
    match (after, a.date.as_deref()) {
        (Some(cut), Some(d)) => d >= cut,
        _ => true,
    }
}

static CAPTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(?:sec|u\.?s\.?\s*securities\s+and\s+exchange\s+commission|securities\s+and\s+exchange\s+commission|united\s+states|u\.?s\.?a?\.?)\s+v\.?s?\.?\s+(.+)$",
    )
    .expect("static caption regex")
});
static MATTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*in\s+the\s+matter\s+of\s+(.+)$").expect("static matter regex"));

/// Corporate suffixes that make a bare release title unambiguously a party.
const ENTITY_SUFFIXES: &[&str] = &[
    "inc", "inc.", "corp", "corp.", "corporation", "llc", "l.l.c.", "llp", "lp", "l.p.", "ltd",
    "ltd.", "limited", "plc", "n.a.", "co", "co.", "company", "gmbh", "s.a.", "ag", "trust",
    "partners", "holdings", "group", "capital", "securities", "advisors", "advisers",
    "management",
];

/// Pull the charged parties out of a release title — only where the title
/// *states* them.
///
/// Three shapes count as stated: a court caption ("SEC v. X", "United States v.
/// X"), an administrative caption ("In the Matter of Y"), and — only where
/// `bare_title_is_caption`, i.e. the SEC feeds, whose title field holds nothing
/// but the parties — a bare title ending in a corporate suffix or an "et al."
/// So "Ichcoin Tech Corp." and "Stephen E. Buyer, et al." yield a party, while
/// "False Forms ADV Filings" — a real SEC litigation-release title that names a
/// topic, not a defendant — correctly yields nothing. Everything else returns
/// empty: a wrong respondent is worse than no respondent, because it reads as a
/// factual claim that a named party was charged.
fn extract_respondents(title: &str, bare_title_is_caption: bool) -> Vec<String> {
    let t = title.trim();
    if t.is_empty() {
        return Vec::new();
    }
    let body = if let Some(c) = CAPTION_RE.captures(t) {
        c.get(1).map(|m| m.as_str().to_string())
    } else if let Some(c) = MATTER_RE.captures(t) {
        c.get(1).map(|m| m.as_str().to_string())
    } else if bare_title_is_caption && bare_title_is_a_party(t) {
        Some(t.to_string())
    } else {
        None
    };
    let Some(body) = body else {
        return Vec::new();
    };
    split_parties(&body)
}

fn bare_title_is_a_party(t: &str) -> bool {
    // A caption is a name, not a sentence. Even inside a feed whose titles are
    // captions, a long one is a topic heading ("Charges Against Three Former
    // Executives of ..."), so cap the length before trusting the suffix.
    const MAX_CAPTION_WORDS: usize = 12;
    if t.split_whitespace().count() > MAX_CAPTION_WORDS {
        return false;
    }
    let lower = t.to_lowercase();
    if lower.contains("et al") {
        return true;
    }
    // Compare the final token against the suffix list, ignoring a trailing
    // comma so "Payward, Inc." matches on "inc.".
    let last = lower
        .rsplit(|c: char| c.is_whitespace())
        .next()
        .unwrap_or_default()
        .trim_matches(|c: char| c == ',' || c == ';');
    ENTITY_SUFFIXES.contains(&last)
}

/// Split only on separators that cannot mean anything but "and also".
///
/// A bare " and " is ambiguous — "Smith and Wesson" is one party — so it is
/// left alone; an Oxford ", and " and a semicolon are not.
fn split_parties(body: &str) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    for semi in body.split(';') {
        if let Some((head, tail)) = semi.split_once(", and ") {
            pieces.extend(head.split(", ").map(str::to_string));
            pieces.push(tail.to_string());
        } else {
            pieces.push(semi.to_string());
        }
    }
    let mut out: Vec<String> = Vec::new();
    for p in pieces {
        let cleaned = clean_party(&p);
        if !cleaned.is_empty() && !out.iter().any(|x| x == &cleaned) {
            out.push(cleaned);
        }
    }
    out
}

fn clean_party(p: &str) -> String {
    let mut s = p.trim().to_string();
    // "X, et al." / "X et al" — the marker is not itself a party.
    let lower = s.to_lowercase();
    for marker in [", et al.", ", et al", " et al.", " et al"] {
        if lower.ends_with(marker) {
            s.truncate(s.len() - marker.len());
            break;
        }
    }
    // Trailing commas and semicolons are list punctuation; a trailing period
    // is not — it belongs to "Corp." and "Inc." and dropping it renames the
    // party.
    s.trim().trim_end_matches([',', ';']).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC_FIXTURE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<rss xmlns:dc="http://purl.org/dc/elements/1.1/" version="2.0" xml:base="https://www.sec.gov/">
  <channel>
    <title>Litigation Releases</title>
    <item>
  <title>Ichcoin Tech Corp.</title>
  <link>https://www.sec.gov/enforcement-litigation/litigation-releases/lr-26623
</link>
  <description>Ichcoin Tech Corp.</description>
  <pubDate>Fri, 28 Aug 2026 17:49:59 -0400</pubDate>
    <dc:creator>LR-26623</dc:creator>
    <guid isPermaLink="false">62253c3e-5b12-4097-a33c-5f0f154ad125</guid>
    </item>
<item>
  <title>SEC v. Jane Q. Public, et al.</title>
  <link>https://www.sec.gov/enforcement-litigation/litigation-releases/lr-26622</link>
  <description>Complaint alleging &amp; describing insider trading.</description>
  <pubDate>Thu, 27 Aug 2026 17:56:23 -0400</pubDate>
    <dc:creator>LR-26622</dc:creator>
    </item>
<item>
  <title>False Forms ADV Filings</title>
  <link>https://www.sec.gov/enforcement-litigation/litigation-releases/lr-26621</link>
  <description><![CDATA[Charges over <b>Form ADV</b>.]]></description>
  <pubDate>Wed, 26 Aug 2026 16:43:52 -0400</pubDate>
    <dc:creator>LR-26621</dc:creator>
    </item>
  </channel>
</rss>"#;

    #[test]
    fn parses_rss_items_from_a_real_feed_shape() {
        let items = parse_rss_items(SEC_FIXTURE);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].title, "Ichcoin Tech Corp.");
        // Trailing newline inside <link> must not survive.
        assert_eq!(
            items[0].link.as_deref(),
            Some("https://www.sec.gov/enforcement-litigation/litigation-releases/lr-26623")
        );
        assert_eq!(items[0].creator.as_deref(), Some("LR-26623"));
        assert_eq!(items[0].date.as_deref(), Some("2026-08-28"));
        // Entities decoded, CDATA unwrapped, inline markup flattened.
        assert_eq!(
            items[1].description.as_deref(),
            Some("Complaint alleging & describing insider trading.")
        );
        assert_eq!(items[2].description.as_deref(), Some("Charges over Form ADV."));
        // A feed with no items must not panic or invent one.
        assert!(parse_rss_items("<rss><channel></channel></rss>").is_empty());
    }

    #[test]
    fn respondents_come_only_from_titles_that_state_them() {
        assert_eq!(
            extract_respondents("SEC v. Jane Q. Public, et al.", true),
            vec!["Jane Q. Public".to_string()]
        );
        assert_eq!(
            extract_respondents("In the Matter of ERHC Energy, Inc.", true),
            vec!["ERHC Energy, Inc.".to_string()]
        );
        assert_eq!(
            extract_respondents("United States v. Acme Holdings LLC", false),
            vec!["Acme Holdings LLC".to_string()]
        );
        assert_eq!(
            extract_respondents("Ichcoin Tech Corp.", true),
            vec!["Ichcoin Tech Corp.".to_string()]
        );
        assert_eq!(
            extract_respondents("Stephen E. Buyer, et al.", true),
            vec!["Stephen E. Buyer".to_string()]
        );
        // A topic headline names no party — must stay empty rather than guess.
        assert!(extract_respondents("False Forms ADV Filings", true).is_empty());
        assert!(extract_respondents(
            "Justice Department Reaffirms Veterinary Accreditation Standards",
            false
        )
        .is_empty());
        // The regression that motivated `bare_title_is_caption`: a DOJ
        // headline that happens to end in a corporate suffix names no party.
        assert!(extract_respondents(
            "Statement of the Antitrust Division on the Closing of Its Investigation of the \
             Merger of Seismic Software Inc. and Highspot Inc.",
            false
        )
        .is_empty());
        // ...and is rejected on length even if a feed claims its titles are
        // captions.
        assert!(extract_respondents(
            "Statement of the Antitrust Division on the Closing of Its Investigation of the \
             Merger of Seismic Software Inc. and Highspot Inc.",
            true
        )
        .is_empty());
        assert!(extract_respondents("", true).is_empty());
    }

    #[test]
    fn party_lists_split_only_on_unambiguous_separators() {
        assert_eq!(
            extract_respondents("SEC v. Alpha Corp., Beta Corp., and Gamma Corp.", true),
            vec![
                "Alpha Corp.".to_string(),
                "Beta Corp.".to_string(),
                "Gamma Corp.".to_string()
            ]
        );
        // A bare " and " may be part of one name — do not split it.
        assert_eq!(
            extract_respondents("SEC v. Smith and Wesson Advisors", true),
            vec!["Smith and Wesson Advisors".to_string()]
        );
    }

    #[test]
    fn sec_ua_is_accepted_only_with_a_contact_address() {
        assert!(looks_like_contact_ua("market-search (me@example.com)"));
        assert!(looks_like_contact_ua("Sample Company AdminContact@domain.com"));
        assert!(!looks_like_contact_ua(
            "legal-search/0.3 (+https://github.com/efoltyn/market-search)"
        ));
        assert!(!looks_like_contact_ua("legal-search/0.3 (contact via github)"));
        assert!(!looks_like_contact_ua("me@localhost"));
        assert!(!looks_like_contact_ua(""));
    }

    #[test]
    fn source_list_expands_all_and_warns_instead_of_failing() {
        let mut w = Vec::new();
        assert_eq!(
            normalize_sources("all", &mut w).expect("all resolves"),
            vec!["sec", "doj", "cfpb"]
        );
        assert!(w.is_empty());

        let mut w = Vec::new();
        assert_eq!(
            normalize_sources("SEC, doj ,sec", &mut w).expect("list resolves"),
            vec!["sec", "doj"]
        );
        assert!(w.is_empty());

        let mut w = Vec::new();
        assert_eq!(
            normalize_sources("doj,epa", &mut w).expect("partial resolves"),
            vec!["doj"]
        );
        assert!(w.iter().any(|x| x.contains("unknown source")));

        // FTC is a known-unavailable source, not a typo — it gets its own note.
        let mut w = Vec::new();
        assert!(normalize_sources("ftc", &mut w).is_err());
        assert!(w.iter().any(|x| x.contains("Akamai")));
    }

    #[test]
    fn doj_rows_are_reordered_newest_first() {
        // Exactly the shape and order DOJ returns: oldest first, epoch strings.
        let rows: serde_json::Value = serde_json::from_str(
            r#"[{"title":"Statement on the Closing of an Antitrust Investigation",
                 "date":"1231156800","url":"https://www.justice.gov/opa/pr/a",
                 "number":"09-001","teaser":"Old one."},
                {"title":"Antitrust Division Secures Commitments",
                 "date":"1779278400","url":"https://www.justice.gov/opa/pr/b",
                 "body":"<p>Newer <b>one</b>.</p>"}]"#,
        )
        .expect("fixture parses");
        let mut actions: Vec<EnforcementAction> = rows
            .as_array()
            .expect("array")
            .iter()
            .filter_map(doj_to_action)
            .collect();
        assert_eq!(actions[0].date.as_deref(), Some("2009-01-05"));
        actions.sort_by(|a, b| b.date.cmp(&a.date));
        assert_eq!(actions[0].date.as_deref(), Some("2026-05-20"));
        assert_eq!(actions[0].source, "doj");
        // HTML body flattened into a readable summary.
        assert_eq!(actions[0].summary.as_deref(), Some("Newer one."));
        assert_eq!(actions[1].release_number.as_deref(), Some("09-001"));
    }

    #[test]
    fn cfpb_rows_are_labelled_as_complaints_not_enforcement() {
        let hit: serde_json::Value = serde_json::from_str(
            r#"{"_id":"15675411","_source":{"product":"Checking or savings account",
                 "issue":"Problem caused by your funds being low",
                 "company":"WELLS FARGO & COMPANY","state":"GA",
                 "date_received":"2025-09-02T15:15:08.000Z",
                 "complaint_what_happened":"Overdraft fee after overdraft fee."}}"#,
        )
        .expect("fixture parses");
        let a = cfpb_to_action(&hit).expect("maps");
        assert_eq!(a.source, "cfpb-complaints");
        assert_eq!(a.date.as_deref(), Some("2025-09-02"));
        assert_eq!(a.release_number.as_deref(), Some("15675411"));
        assert_eq!(a.respondents, vec!["WELLS FARGO & COMPANY".to_string()]);
        assert!(a.title.as_deref().unwrap().starts_with("WELLS FARGO"));
    }

    #[test]
    fn date_and_keyword_filters_are_inclusive_and_case_insensitive() {
        let a = EnforcementAction {
            source: "sec-litigation".into(),
            title: Some("SEC v. Insider Trading Defendants".into()),
            date: Some("2026-08-28".into()),
            ..Default::default()
        };
        assert!(after_ok(&a, Some("2026-08-28")));
        assert!(after_ok(&a, Some("2026-01-01")));
        assert!(!after_ok(&a, Some("2026-08-29")));
        // A row with no date survives filtering rather than vanishing silently.
        let undated = EnforcementAction {
            date: None,
            ..a.clone()
        };
        assert!(after_ok(&undated, Some("2030-01-01")));

        assert!(matches_query(&a, Some("INSIDER")));
        assert!(matches_query(&a, None));
        assert!(!matches_query(&a, Some("municipal bonds")));
    }

    #[test]
    fn upstream_timestamp_formats_all_reduce_to_one_sortable_day() {
        assert_eq!(
            rfc2822_to_day("Fri, 28 Aug 2026 17:49:59 -0400").as_deref(),
            Some("2026-08-28")
        );
        assert_eq!(rfc2822_to_day("not a date"), None);
        assert_eq!(epoch_to_day("1231156800").as_deref(), Some("2009-01-05"));
        assert_eq!(epoch_to_day(""), None);
        assert_eq!(iso_day("2025-09-02T15:15:08.000Z"), "2025-09-02");
    }
}
