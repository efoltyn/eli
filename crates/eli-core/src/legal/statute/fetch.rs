//! Statutes and bills — the US Code as enacted, and legislation in flight.
//!
//! Two things here are hard to get any other way:
//!   * **A citation resolves to one document, deterministically.** "15 U.S.C.
//!     § 78j" goes through the govinfo link service to exactly one GPO granule
//!     — no ranking, no near-miss, no law-firm blog post that paraphrases the
//!     section and drops the 2010 Dodd-Frank amendments. The commercial
//!     statute sites that rank well are unofficial copies of unknown vintage;
//!     this is the Office of Law Revision Counsel's own text, with the edition
//!     year and the source credit (every public law that amended the section)
//!     attached.
//!   * **A bill's status is a moving target with no stable page.** Whether
//!     H.R. 3076 is a bill, a passed-House bill or Public Law 117-108 changed
//!     four times in eleven months; a search engine holds whichever version it
//!     last crawled. GovTrack + govinfo BILLSTATUS give the current state,
//!     sponsor, introduction date and the CRS summary as fields.
//!
//! No API key on either path. congress.gov's own API needs one and has no
//! full-text search; its HTML is Cloudflare-blocked. govinfo's *bulkdata* tree
//! and *link* service are key-free, while `api.govinfo.gov` 401s without a key
//! — so everything below deliberately stays on the key-free endpoints.

use crate::legal::{clamp_text, shared_client, soft_fail, strip_markup};
use crate::{Error, Result};
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

const GOVINFO_LINK: &str = "https://www.govinfo.gov/link";
const GOVTRACK_BILL: &str = "https://www.govtrack.us/api/v2/bill";
const BILLSTATUS_BULK: &str = "https://www.govinfo.gov/bulkdata/BILLSTATUS";

/// govinfo's BILLSTATUS bulk tree starts at the 113th Congress; older bills
/// exist on GovTrack but have no XML to enrich them with.
const BILLSTATUS_EARLIEST_CONGRESS: u32 = 113;

/// Only ask GovTrack for the fields we render. The full object carries
/// `major_actions` and a nested `sponsor_role` — tens of KB per bill for
/// things this response never shows.
const GOVTRACK_FIELDS: &str = "congress,bill_type,number,display_number,title,title_without_number,\
current_status,current_status_label,current_status_date,current_status_description,introduced_date,\
link,sponsor,sliplawnum,sliplawpubpriv,is_alive";

#[derive(Clone, Debug)]
pub struct StatuteRequest {
    pub title: Option<u32>,
    pub section: Option<String>,
    pub congress: Option<u32>,
    pub bill: Option<String>,
    pub max_chars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StatuteResponse {
    pub generated_at: DateTime<Utc>,
    /// Which path answered: "uscode" | "uscode_title" | "bill".
    pub mode: String,
    pub citation: Option<String>,
    pub heading: Option<String>,
    pub text: Option<String>,
    pub chars: usize,
    pub truncated: bool,
    pub source_url: Option<String>,

    // US Code mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub us_code_title: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Which annual edition of the Code the text came from (govinfo defaults
    /// to the latest); the text is only current through that edition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edition: Option<String>,
    /// Every public law that amended the section, verbatim from GPO.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_credit: Option<String>,

    // Bill mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub congress: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bill_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sponsor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub introduced_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_law: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_area: Option<String>,
    /// Landing pages for the same document — GovTrack, govinfo XML, congress.gov.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related_urls: Vec<String>,

    pub warnings: Vec<String>,
}

impl StatuteResponse {
    fn empty(mode: &str) -> Self {
        Self {
            generated_at: Utc::now(),
            mode: mode.to_string(),
            citation: None,
            heading: None,
            text: None,
            chars: 0,
            truncated: false,
            source_url: None,
            us_code_title: None,
            section: None,
            edition: None,
            source_credit: None,
            congress: None,
            bill_number: None,
            status: None,
            status_date: None,
            sponsor: None,
            introduced_date: None,
            latest_action: None,
            public_law: None,
            policy_area: None,
            related_urls: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn set_text(&mut self, body: &str, max_chars: usize) {
        let (clamped, truncated) = clamp_text(body, max_chars);
        self.chars = body.chars().count();
        self.truncated = truncated;
        self.text = Some(clamped);
    }
}

pub async fn fetch_statute(req: StatuteRequest) -> Result<StatuteResponse> {
    // Bill wins when both are given: `--bill` is the more specific ask, and
    // silently ignoring it would answer a question nobody asked.
    if let Some(bill) = req.bill.clone().filter(|b| !b.trim().is_empty()) {
        return bill_mode(&bill, &req).await;
    }
    match req.title {
        Some(title) => us_code_mode(title, &req).await,
        None => Err(Error::InvalidInput(
            "statute requires --title (with --section) or --bill".into(),
        )),
    }
}

// ── US Code ────────────────────────────────────────────────────────────────

async fn us_code_mode(title: u32, req: &StatuteRequest) -> Result<StatuteResponse> {
    let section = req
        .section
        .as_deref()
        .map(normalize_section)
        .filter(|s| !s.is_empty());
    let Some(section) = section else {
        return Ok(us_code_title_only(title));
    };

    let mut out = StatuteResponse::empty("uscode");
    out.us_code_title = Some(title);
    out.section = Some(section.clone());
    out.citation = Some(citation_of(title, &section));

    let url = uscode_link_url(title, &section);
    out.source_url = Some(url.clone());
    out.related_urls
        .push(format!("{GOVINFO_LINK}/uscode/{title}/{section}?link-type=pdf"));

    let Some(body) = fetch_document(&url, "govinfo link (uscode)", &mut out.warnings).await else {
        return Ok(out);
    };

    // The link service also answers *200* with a wrapper page when the granule
    // is missing, so the status code alone is not proof we got statute text.
    if let Some(reason) = error_page_reason(&body) {
        out.warnings.push(format!(
            "govinfo returned {reason} for {} rather than the section text — check the section \
             number (US Code sections are like \"78j\" or \"1681a\", not \"10b-5\")",
            out.citation.clone().unwrap_or_default()
        ));
        return Ok(out);
    }

    let Some(doc) = parse_uscode_html(&body) else {
        out.warnings.push(
            "govinfo granule had no recognizable statute body; fetch the source_url directly"
                .to_string(),
        );
        return Ok(out);
    };

    out.heading = doc.heading;
    out.edition = doc.edition;
    out.source_credit = doc.source_credit;
    if doc.notes_elided {
        out.warnings.push(
            "editorial and amendment notes were elided; they are on the source_url page"
                .to_string(),
        );
    }
    out.set_text(&doc.text, req.max_chars);
    Ok(out)
}

/// A title with no section. There is no single "title text" document worth
/// fetching (title 42 is tens of megabytes), so hand back the title's name and
/// the browse entry point rather than an empty object.
fn us_code_title_only(title: u32) -> StatuteResponse {
    let mut out = StatuteResponse::empty("uscode_title");
    out.us_code_title = Some(title);
    out.citation = Some(format!("{title} U.S.C."));
    out.heading = us_code_title_name(title).map(str::to_string);
    out.source_url = Some(format!(
        "https://uscode.house.gov/browse/prelim@title{title}&edition=prelim"
    ));
    out.related_urls
        .push(format!("https://www.govinfo.gov/app/collection/uscode"));
    match out.heading.as_deref() {
        Some(name) => out.warnings.push(format!(
            "--title {title} is \"{name}\"; add --section for the statutory text (e.g. --title \
             {title} --section {})",
            example_section(title)
        )),
        None => out.warnings.push(format!(
            "no name on record for US Code title {title} (valid titles are 1-54, and 53 is \
             partly unenacted); add --section for statutory text"
        )),
    }
    out
}

/// "15 U.S.C. § 78j" — the form a brief or a court would use.
fn citation_of(title: u32, section: &str) -> String {
    format!("{title} U.S.C. § {section}")
}

/// Callers paste citations in every shape: "§ 78j", "Sec. 78j", "78j.".
fn normalize_section(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .trim_start_matches('§')
        .trim()
        .trim_end_matches('.')
        .to_string();
    let lower = cleaned.to_ascii_lowercase();
    let stripped = lower
        .strip_prefix("sec.")
        .or_else(|| lower.strip_prefix("sec "))
        .or_else(|| lower.strip_prefix("section "))
        .map(|_| {
            // Preserve the original casing of the number itself.
            let cut = cleaned.len() - cleaned.trim_start_matches(|c: char| c.is_ascii_alphabetic() || c == '.' || c == ' ').len();
            cleaned[cut..].to_string()
        })
        .unwrap_or(cleaned);
    stripped.trim().replace(' ', "")
}

/// For US Code the section goes **in the path**. This is the opposite of the
/// CFR endpoint on the same service, where a dotted path (`/link/cfr/17/240.10b-5`)
/// is parsed as a part number and 400s — there the section must ride in
/// `?sectionnum=10b-5`. Same host, two different conventions.
fn uscode_link_url(title: u32, section: &str) -> String {
    format!(
        "{GOVINFO_LINK}/uscode/{title}/{}?link-type=html",
        urlencoding::encode(section)
    )
}

struct UsCodeDoc {
    heading: Option<String>,
    edition: Option<String>,
    text: String,
    source_credit: Option<String>,
    notes_elided: bool,
}

/// GPO's US Code granules are HTML with the structure marked in comments
/// (`<!-- field-start:statute -->` … `<!-- field-end:statute -->`). Slicing on
/// those is exact; slicing on the CSS classes is not, because the notes use
/// the same `<p class="statutory-body">` markup as the operative text.
fn parse_uscode_html(html: &str) -> Option<UsCodeDoc> {
    let statute = between(html, "<!-- field-start:statute -->", "<!-- field-end:statute -->")?;
    let text = plain(statute);
    if text.is_empty() {
        return None;
    }
    let heading = between(html, "<!-- field-start:head -->", "<!-- field-end:head -->")
        .map(plain)
        .filter(|h| !h.is_empty());
    // Header block, e.g. "United States Code, 2024 Edition".
    let edition = html
        .find("United States Code, ")
        .map(|i| plain(&html[i..html.len().min(i + 60)]))
        .and_then(|s| s.split('<').next().map(str::to_string))
        .map(|s| s.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()).to_string())
        .filter(|s| s.contains("Edition"));
    let source_credit = between(
        html,
        "<!-- field-start:sourcecredit -->",
        "<!-- field-end:sourcecredit -->",
    )
    .map(plain)
    .filter(|s| !s.is_empty());
    Some(UsCodeDoc {
        heading,
        edition,
        text,
        source_credit,
        notes_elided: html.contains("<!-- field-start:notes -->"),
    })
}

// ── Bills ──────────────────────────────────────────────────────────────────

async fn bill_mode(raw: &str, req: &StatuteRequest) -> Result<StatuteResponse> {
    let mut out = StatuteResponse::empty("bill");
    let parsed = parse_bill_id(raw);

    let url = match &parsed {
        Some(id) => {
            let mut u = format!(
                "{GOVTRACK_BILL}?bill_type={}&number={}&sort=-congress&limit=5&fields={GOVTRACK_FIELDS}",
                id.govtrack_type, id.number
            );
            if let Some(c) = req.congress {
                u.push_str(&format!("&congress={c}"));
            }
            u
        }
        None => {
            // Not a bill number — treat it as a title search. GovTrack is the
            // only key-free federal source with real full-text search.
            out.warnings.push(format!(
                "{raw:?} is not a bill number (expected e.g. \"hr3076\" or \"s1720\"); searched \
                 bill titles for it instead"
            ));
            let mut u = format!(
                "{GOVTRACK_BILL}?q={}&sort=-congress&limit=5&fields={GOVTRACK_FIELDS}",
                urlencoding::encode(raw.trim())
            );
            if let Some(c) = req.congress {
                u.push_str(&format!("&congress={c}"));
            }
            u
        }
    };

    let govtrack = fetch_json(&url, "govtrack bill", &mut out.warnings).await;
    let hit = govtrack
        .as_ref()
        .and_then(|v| v.get("objects"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first());
    let total = govtrack
        .as_ref()
        .and_then(|v| v.get("meta"))
        .and_then(|m| m.get("total_count"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0);

    if let Some(bill) = hit {
        apply_govtrack(bill, &mut out);
        // Bill numbers repeat every Congress, so "H.R. 3076" alone is
        // ambiguous — say which one we picked and how many others exist.
        if req.congress.is_none() && total > 1 {
            out.warnings.push(format!(
                "no --congress given: took the most recent match ({}), but {total} bills share \
                 this number across Congresses — pass --congress to pin one",
                out.congress
                    .map(|c| format!("{} Congress", ordinal(c)))
                    .unwrap_or_else(|| "latest".to_string())
            ));
        }
    } else if govtrack.is_some() {
        out.warnings.push(format!(
            "GovTrack has no bill matching {raw:?}{}",
            req.congress
                .map(|c| format!(" in the {} Congress", ordinal(c)))
                .unwrap_or_default()
        ));
    }

    // Fall back to the requested Congress so BILLSTATUS can still be tried
    // when GovTrack itself was unreachable.
    let congress = out.congress.or(req.congress).or_else(|| {
        parsed.as_ref().map(|_| {
            let c = current_congress();
            out.warnings
                .push(format!("no --congress given; assumed the current ({})", ordinal(c)));
            c
        })
    });
    out.congress = congress;

    if out.citation.is_none() {
        if let (Some(id), Some(c)) = (parsed.as_ref(), congress) {
            out.citation = Some(format!("{} {} ({} Congress)", id.label, id.number, ordinal(c)));
            out.bill_number = Some(format!("{} {}", id.label, id.number));
        }
    }

    // govinfo BILLSTATUS: the CRS summary, policy area, latest action and the
    // public law number — none of which GovTrack exposes in this shape.
    if let (Some(id), Some(c)) = (parsed.as_ref(), congress) {
        if c < BILLSTATUS_EARLIEST_CONGRESS {
            out.warnings.push(format!(
                "govinfo BILLSTATUS starts at the {} Congress; no summary available for the {}",
                ordinal(BILLSTATUS_EARLIEST_CONGRESS),
                ordinal(c)
            ));
        } else {
            let xml_url = billstatus_url(c, &id.code, id.number);
            out.related_urls.push(xml_url.clone());
            if let Some(xml) = fetch_text(&xml_url, "govinfo BILLSTATUS", &mut out.warnings).await {
                apply_billstatus(&xml, &mut out);
            }
        }
    }

    // Prefer the CRS summary; GovTrack's status sentence is the floor.
    if let Some(raw) = out.text.take() {
        out.set_text(&raw, req.max_chars);
    }
    if out.text.is_none() {
        if let Some(desc) = out.status.clone().zip(out.status_date.clone()).map(|(s, d)| {
            format!("Status: {s} (as of {d}).")
        }) {
            out.warnings
                .push("no CRS summary available; returning the status line only".to_string());
            out.set_text(&desc, req.max_chars);
        }
    }
    if out.citation.is_none() && out.text.is_none() && out.warnings.is_empty() {
        out.warnings
            .push(format!("nothing found for --bill {raw:?}"));
    }
    Ok(out)
}

fn apply_govtrack(bill: &serde_json::Value, out: &mut StatuteResponse) {
    let s = |k: &str| bill.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let congress = bill.get("congress").and_then(|v| v.as_u64()).map(|c| c as u32);
    out.congress = congress;
    out.bill_number = s("display_number");
    out.heading = s("title_without_number").or_else(|| s("title"));
    out.status = s("current_status_label").or_else(|| s("current_status"));
    out.status_date = s("current_status_date");
    out.introduced_date = s("introduced_date");
    out.sponsor = bill
        .get("sponsor")
        .and_then(|sp| sp.get("name"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    out.latest_action = s("current_status_description");
    if let Some(link) = s("link") {
        out.related_urls.push(link);
    }
    if let (Some(num), Some(c)) = (out.bill_number.clone(), congress) {
        out.citation = Some(format!("{num} ({} Congress)", ordinal(c)));
    }
    // sliplawnum + PUB/PRIV is GovTrack's encoding of "this became law".
    if let (Some(n), Some(c)) = (
        bill.get("sliplawnum").and_then(|v| v.as_u64()),
        congress,
    ) {
        let kind = match bill.get("sliplawpubpriv").and_then(|v| v.as_str()) {
            Some("PRIV") => "Private Law",
            _ => "Public Law",
        };
        out.public_law = Some(format!("{kind} {c}-{n}"));
    }
    out.source_url = out.related_urls.first().cloned();
}

/// Pull the fields worth having out of a BILLSTATUS XML record. Hand-sliced
/// rather than parsed: the file is a flat, stable schema (version 3.0.0) and
/// pulling four elements out of it does not justify an XML dependency.
fn apply_billstatus(xml: &str, out: &mut StatuteResponse) {
    if out.policy_area.is_none() {
        out.policy_area = xml_block(xml, "policyArea").and_then(|b| xml_field(b, "name"));
    }
    if let Some(law) = xml_block(xml, "laws").and_then(|b| {
        let kind = xml_field(b, "type")?;
        let num = xml_field(b, "number")?;
        Some(format!("{kind} {num}"))
    }) {
        out.public_law = Some(law);
    }
    // The bill-level latestAction is the last <latestAction> block in the
    // file; the earlier ones belong to individual actions and committees.
    if let Some(action) = xml_blocks(xml, "latestAction")
        .last()
        .and_then(|b| Some(format!("{} ({})", xml_field(b, "text")?, xml_field(b, "actionDate")?)))
    {
        out.latest_action = Some(action);
    }
    if out.heading.is_none() {
        out.heading = xml_field(xml, "title");
    }

    // Summaries accumulate one per stage; the newest describes the bill as it
    // now stands, which is what a reader asking about the bill wants.
    let summaries = xml_blocks(xml, "summary");
    let newest = summaries
        .iter()
        .max_by_key(|b| xml_field(b, "actionDate").unwrap_or_default());
    if let Some(block) = newest {
        if let Some(text) = xml_field(block, "text").filter(|t| !t.is_empty()) {
            let stage = xml_field(block, "actionDesc").unwrap_or_else(|| "summary".to_string());
            let body = format!("[CRS summary — {stage}] {text}");
            // max_chars is applied by the caller after this; store raw and let
            // bill_mode clamp. Clamping twice would double-count `chars`.
            out.text = Some(body);
        }
    }
}

struct BillId {
    /// govinfo/bulkdata directory name and file infix: "hr", "s", "hjres".
    code: &'static str,
    /// GovTrack's `bill_type` enum value.
    govtrack_type: &'static str,
    /// Citation form: "H.R.", "S.", "H.J.Res.".
    label: &'static str,
    number: u32,
}

/// "hr3076" / "H.R. 3076" / "s.1720" -> type + number. Returns None for
/// anything that isn't a bill designation so the caller can fall back to a
/// title search instead of guessing.
fn parse_bill_id(raw: &str) -> Option<BillId> {
    let compact: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    let split = compact.find(|c: char| c.is_ascii_digit())?;
    let (prefix, digits) = compact.split_at(split);
    let number: u32 = digits.parse().ok()?;
    let (code, govtrack_type, label) = match prefix {
        "hr" => ("hr", "house_bill", "H.R."),
        "s" => ("s", "senate_bill", "S."),
        "hres" => ("hres", "house_resolution", "H.Res."),
        "sres" => ("sres", "senate_resolution", "S.Res."),
        "hjres" => ("hjres", "house_joint_resolution", "H.J.Res."),
        "sjres" => ("sjres", "senate_joint_resolution", "S.J.Res."),
        "hconres" => ("hconres", "house_concurrent_resolution", "H.Con.Res."),
        "sconres" => ("sconres", "senate_concurrent_resolution", "S.Con.Res."),
        _ => return None,
    };
    Some(BillId {
        code,
        govtrack_type,
        label,
        number,
    })
}

fn billstatus_url(congress: u32, code: &str, number: u32) -> String {
    format!("{BILLSTATUS_BULK}/{congress}/{code}/BILLSTATUS-{congress}{code}{number}.xml")
}

/// A Congress spans two years and is seated on January 3 of the odd year.
fn current_congress() -> u32 {
    let now = Utc::now().date_naive();
    let mut year = now.year();
    if year % 2 == 1 && now.month() == 1 && now.day() < 3 {
        year -= 1; // The previous Congress is still sitting.
    }
    (((year - 1789) / 2) + 1) as u32
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Fetch a document, distinguishing "upstream said no" from "upstream handed
/// back its own error page". soft_fail can't do the latter: it only sees a
/// status code, and govinfo's link service reports bad citations as a 400
/// carrying 68 KB of HTML chrome.
async fn fetch_document(url: &str, source: &str, warnings: &mut Vec<String>) -> Option<String> {
    let resp = match shared_client::GENERAL.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("{source} request failed: {e}"));
            return None;
        }
    };
    let status = resp.status();
    if status.is_success() {
        return match resp.text().await {
            Ok(t) => Some(t),
            Err(e) => {
                warnings.push(format!("{source} body read failed: {e}"));
                None
            }
        };
    }
    let body = resp.text().await.unwrap_or_default();
    warnings.push(match error_page_reason(&body) {
        Some(reason) => format!("{source}: {status} — {reason}"),
        None => {
            let snippet: String = body.chars().take(200).collect();
            format!("{source}: {status} ({snippet})")
        }
    });
    None
}

async fn fetch_text(url: &str, source: &str, warnings: &mut Vec<String>) -> Option<String> {
    let resp = match shared_client::BULK.get(url).send().await {
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

async fn fetch_json(
    url: &str,
    source: &str,
    warnings: &mut Vec<String>,
) -> Option<serde_json::Value> {
    let body = fetch_text(url, source, warnings).await?;
    match serde_json::from_str(&body) {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("{source} returned unparseable JSON: {e}"));
            None
        }
    }
}

/// Is this body an error page dressed as a document?
///
/// Two measured failure modes, neither detectable by status code:
///   * govinfo's link service answers a bad citation with `Govinfo Link
///     Service Error` — a full-size HTML page, sometimes with a 200.
///   * uscode.house.gov answers a guessed release-point URL with **HTTP 200**
///     and a ~3.8 KB HTML stub. Nothing but the absence of document markers
///     distinguishes it from a real granule.
fn error_page_reason(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    if lower.contains("link service error") {
        return Some("the govinfo Link Service error page".to_string());
    }
    let has_document = lower.contains("field-start:statute")
        || lower.contains("field-start:head")
        || lower.contains("section-head");
    if has_document {
        return None;
    }
    if lower.contains("<html") || lower.contains("<!doctype html") {
        return Some(format!(
            "an HTML page with no statute markup ({} bytes)",
            body.len()
        ));
    }
    body.trim().is_empty().then(|| "an empty body".to_string())
}

/// Slice the content between two literal markers.
fn between<'a>(haystack: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = haystack.find(start)? + start.len();
    let rest = &haystack[from..];
    let to = rest.find(end)?;
    Some(&rest[..to])
}

/// Markup -> words. `strip_markup` handles tags and the common entities; GPO
/// text is full of `&sect;`, `&ndash;` and `&mdash;`, so decode the rest after
/// the tags are gone (decoding first would create tags that then get stripped).
fn plain(markup: &str) -> String {
    let stripped = strip_markup(markup);
    let decoded = html_escape::decode_html_entities(&stripped);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn xml_block<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    between(xml, &format!("<{tag}>"), &format!("</{tag}>"))
}

fn xml_blocks<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(&after[..j]);
        rest = &after[j + close.len()..];
    }
    out
}

fn xml_field(xml: &str, tag: &str) -> Option<String> {
    let raw = xml_block(xml, tag)?;
    let raw = raw
        .trim()
        .trim_start_matches("<![CDATA[")
        .trim_end_matches("]]>");
    let text = plain(raw);
    (!text.is_empty()).then_some(text)
}

/// Enacted US Code titles. Used only to make a title-without-section request
/// informative; the text always comes from GPO.
fn us_code_title_name(title: u32) -> Option<&'static str> {
    Some(match title {
        1 => "General Provisions",
        2 => "The Congress",
        3 => "The President",
        4 => "Flag and Seal, Seat of Government, and the States",
        5 => "Government Organization and Employees",
        6 => "Domestic Security",
        7 => "Agriculture",
        8 => "Aliens and Nationality",
        9 => "Arbitration",
        10 => "Armed Forces",
        11 => "Bankruptcy",
        12 => "Banks and Banking",
        13 => "Census",
        14 => "Coast Guard",
        15 => "Commerce and Trade",
        16 => "Conservation",
        17 => "Copyrights",
        18 => "Crimes and Criminal Procedure",
        19 => "Customs Duties",
        20 => "Education",
        21 => "Food and Drugs",
        22 => "Foreign Relations and Intercourse",
        23 => "Highways",
        24 => "Hospitals and Asylums",
        25 => "Indians",
        26 => "Internal Revenue Code",
        27 => "Intoxicating Liquors",
        28 => "Judiciary and Judicial Procedure",
        29 => "Labor",
        30 => "Mineral Lands and Mining",
        31 => "Money and Finance",
        32 => "National Guard",
        33 => "Navigation and Navigable Waters",
        34 => "Crime Control and Law Enforcement",
        35 => "Patents",
        36 => "Patriotic and National Observances, Ceremonies, and Organizations",
        37 => "Pay and Allowances of the Uniformed Services",
        38 => "Veterans' Benefits",
        39 => "Postal Service",
        40 => "Public Buildings, Property, and Works",
        41 => "Public Contracts",
        42 => "The Public Health and Welfare",
        43 => "Public Lands",
        44 => "Public Printing and Documents",
        45 => "Railroads",
        46 => "Shipping",
        47 => "Telecommunications",
        48 => "Territories and Insular Possessions",
        49 => "Transportation",
        50 => "War and National Defense",
        51 => "National and Commercial Space Programs",
        52 => "Voting and Elections",
        54 => "National Park Service and Related Programs",
        _ => return None,
    })
}

/// A section that actually exists in the title, so the "add --section" warning
/// carries a copy-pasteable example instead of a placeholder.
fn example_section(title: u32) -> &'static str {
    match title {
        15 => "78j",
        17 => "107",
        18 => "1001",
        26 => "61",
        _ => "1",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_us_code_citations() {
        assert_eq!(citation_of(15, "78j"), "15 U.S.C. § 78j");
        assert_eq!(citation_of(17, "107"), "17 U.S.C. § 107");
    }

    #[test]
    fn normalizes_pasted_sections() {
        assert_eq!(normalize_section(" § 78j "), "78j");
        assert_eq!(normalize_section("Sec. 107"), "107");
        assert_eq!(normalize_section("1681a."), "1681a");
        assert_eq!(normalize_section("78j-1"), "78j-1");
    }

    #[test]
    fn uscode_section_goes_in_the_path_not_a_query_param() {
        // The CFR half of the same service demands ?sectionnum= and 400s on a
        // dotted path; US Code is the reverse. Guard against copying the CFR
        // convention over here.
        let url = uscode_link_url(15, "78j");
        assert_eq!(
            url,
            "https://www.govinfo.gov/link/uscode/15/78j?link-type=html"
        );
        assert!(!url.contains("sectionnum"));
    }

    #[test]
    fn rejects_html_error_pages_but_keeps_real_granules() {
        // uscode.house.gov's silent-200 stub: ~3.8 KB of HTML with no markers.
        let stub = format!(
            "<html><head><title>Error</title></head><body><p>{}</p></body></html>",
            "The requested page was not found. ".repeat(105)
        );
        assert!(stub.len() > 3_500 && stub.len() < 4_100);
        assert!(error_page_reason(&stub).is_some());

        // The link service's own error page, whatever its status code.
        assert!(error_page_reason("<html><body>Govinfo Link Service Error</body></html>").is_some());

        // A real granule passes.
        let real = r#"<html><body><!-- field-start:head -->
<h3 class="section-head">&sect;78j. Manipulative and deceptive devices</h3>
<!-- field-end:head --><!-- field-start:statute -->
<p class="statutory-body">It shall be unlawful&mdash;</p>
<!-- field-end:statute --></body></html>"#;
        assert!(error_page_reason(real).is_none());
    }

    #[test]
    fn parses_a_uscode_granule() {
        let html = r#"<html><body>
<span style="font-size:10pt">United States Code, 2024 Edition</span><br/>
<!-- field-start:head -->
<h3 class="section-head">&sect;78j. Manipulative and deceptive devices</h3>
<!-- field-end:head -->
<!-- field-start:statute -->
<p class="statutory-body">It shall be unlawful for any person&mdash;</p>
<!-- field-end:statute -->
<!-- field-start:sourcecredit -->
<p class="source-credit">(June 6, 1934, ch. 404, &sect;10, 48 Stat. 891.)</p>
<!-- field-end:sourcecredit -->
<!-- field-start:notes --><p>Editorial notes</p><!-- field-end:notes -->
</body></html>"#;
        let doc = parse_uscode_html(html).expect("granule parses");
        assert_eq!(
            doc.heading.as_deref(),
            Some("§78j. Manipulative and deceptive devices")
        );
        assert_eq!(doc.text, "It shall be unlawful for any person—");
        assert_eq!(doc.edition.as_deref(), Some("United States Code, 2024 Edition"));
        assert!(doc.source_credit.unwrap().contains("48 Stat. 891"));
        assert!(doc.notes_elided);
    }

    #[test]
    fn parses_bill_ids() {
        let id = parse_bill_id("hr3076").expect("hr3076");
        assert_eq!(id.code, "hr");
        assert_eq!(id.govtrack_type, "house_bill");
        assert_eq!(id.number, 3076);

        let id = parse_bill_id("S. 1720").expect("S. 1720");
        assert_eq!(id.code, "s");
        assert_eq!(id.label, "S.");
        assert_eq!(id.number, 1720);

        assert_eq!(parse_bill_id("H.J.Res. 1").map(|i| i.code), Some("hjres"));
        assert_eq!(parse_bill_id("sconres5").map(|i| i.govtrack_type), Some("senate_concurrent_resolution"));
        // Not a bill number — must fall through to a title search.
        assert!(parse_bill_id("postal service reform").is_none());
        assert!(parse_bill_id("xyz12").is_none());
    }

    #[test]
    fn builds_billstatus_bulk_urls() {
        assert_eq!(
            billstatus_url(117, "hr", 3076),
            "https://www.govinfo.gov/bulkdata/BILLSTATUS/117/hr/BILLSTATUS-117hr3076.xml"
        );
    }

    #[test]
    fn congress_defaults_to_the_one_now_sitting() {
        let c = current_congress();
        // 119th Congress = 2025-2026; the arithmetic must track the calendar,
        // not a hard-coded number.
        let expected = (((Utc::now().year() - 1789) / 2) + 1) as u32;
        assert!(c == expected || c == expected - 1);
        assert!(c >= 119);
    }

    #[test]
    fn ordinals_handle_the_teens() {
        assert_eq!(ordinal(117), "117th");
        assert_eq!(ordinal(121), "121st");
        assert_eq!(ordinal(122), "122nd");
        assert_eq!(ordinal(123), "123rd");
        assert_eq!(ordinal(113), "113th");
        assert_eq!(ordinal(111), "111th");
    }

    #[test]
    fn maps_govtrack_fields_onto_the_response() {
        let bill = serde_json::json!({
            "congress": 117,
            "display_number": "H.R. 3076",
            "title_without_number": "Postal Service Reform Act of 2022",
            "current_status_label": "Enacted — Signed by the President",
            "current_status_date": "2022-04-06",
            "introduced_date": "2021-05-11",
            "sponsor": {"name": "Rep. Carolyn Maloney [D-NY12, 2013-2022]"},
            "sliplawnum": 108,
            "sliplawpubpriv": "PUB",
            "link": "https://www.govtrack.us/congress/bills/117/hr3076"
        });
        let mut out = StatuteResponse::empty("bill");
        apply_govtrack(&bill, &mut out);
        assert_eq!(out.citation.as_deref(), Some("H.R. 3076 (117th Congress)"));
        assert_eq!(out.public_law.as_deref(), Some("Public Law 117-108"));
        assert_eq!(out.congress, Some(117));
        assert!(out.sponsor.unwrap().contains("Maloney"));
    }

    #[test]
    fn pulls_summary_and_law_out_of_billstatus_xml() {
        let xml = r#"<billStatus><bill><number>3076</number>
<policyArea><name>Government Operations and Politics</name></policyArea>
<actions><item><latestAction><actionDate>2021-05-19</actionDate><text>Referred.</text></latestAction></item></actions>
<latestAction><actionDate>2022-04-06</actionDate><text>Became Public Law No: 117-108.</text></latestAction>
<laws><item><type>Public Law</type><number>117-108</number></item></laws>
<summaries>
<summary><actionDate>2021-05-11</actionDate><actionDesc>Introduced in House</actionDesc><text><![CDATA[<p>Older summary.</p>]]></text></summary>
<summary><actionDate>2022-03-08</actionDate><actionDesc>Passed Senate</actionDesc><text><![CDATA[<p>This bill addresses USPS finances.</p>]]></text></summary>
</summaries></bill></billStatus>"#;
        let mut out = StatuteResponse::empty("bill");
        apply_billstatus(xml, &mut out);
        assert_eq!(out.public_law.as_deref(), Some("Public Law 117-108"));
        assert_eq!(
            out.policy_area.as_deref(),
            Some("Government Operations and Politics")
        );
        // The bill-level latestAction, not the one nested inside an action.
        assert_eq!(
            out.latest_action.as_deref(),
            Some("Became Public Law No: 117-108. (2022-04-06)")
        );
        let text = out.text.expect("summary");
        assert!(text.contains("Passed Senate"));
        assert!(text.contains("USPS finances"));
        assert!(!text.contains("Older summary"));
    }

    #[test]
    fn title_without_section_still_answers() {
        let out = us_code_title_only(15);
        assert_eq!(out.mode, "uscode_title");
        assert_eq!(out.heading.as_deref(), Some("Commerce and Trade"));
        assert_eq!(out.citation.as_deref(), Some("15 U.S.C."));
        assert!(out.warnings.iter().any(|w| w.contains("--section")));
        // An unknown title warns instead of pretending.
        assert!(us_code_title_only(99)
            .warnings
            .iter()
            .any(|w| w.contains("no name on record")));
    }
}
