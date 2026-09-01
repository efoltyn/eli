//! Colorado business entities — who to serve, and where.
//!
//! This exists for two moments in a Colorado personal-injury case, both of
//! which turn on the *registered agent* rather than on anything a web search
//! returns:
//!
//!   * **The C.R.S. 10-3-1117 insurance-disclosure demand.** The statute gives
//!     an insurer 30 days to disclose coverage and imposes $100/day for late
//!     compliance — but only from service on the entity's registered agent. A
//!     demand mailed to a claims office is not service, so the clock and the
//!     penalty never start. Getting the agent's name and street address right
//!     is the whole game.
//!   * **Service of process** on a corporate or insurer defendant under
//!     C.R.C.P. 4(e)(4).
//!
//! What a web search cannot give you: the *current* agent of record and the
//! entity's *status*. Searching "State Farm registered agent Colorado" returns
//! blog posts and stale directory scrapes; agents change, and serving a
//! dissolved or delinquent shell is a real and expensive failure mode. This
//! reads the Secretary of State's own register.
//!
//! Source: the SOS publishes the full entity register as a key-free Socrata
//! dataset, refreshed daily:
//! https://data.colorado.gov/resource/4ykn-tg5h.json
//!
//! The mirror is a snapshot, not the register. Every response says so, because
//! "the data was a day old" is not a defense to a blown service.

use super::{EntityRecord, EntityRequest, EntityResponse};
use crate::legal::{shared_client, soft_fail};
use crate::{Error, Result};

/// Socrata resource endpoint. No app token needed; anonymous requests are
/// throttled per-IP but generously enough for interactive lookups.
const SOCRATA: &str = "https://data.colorado.gov/resource/4ykn-tg5h.json";
/// The human dataset page — what `source_url` points at.
const DATASET_PAGE: &str =
    "https://data.colorado.gov/Business/Business-Entities-in-Colorado/4ykn-tg5h";
/// The authoritative record. `www.coloradosos.gov` serves it; the older
/// `www.sos.state.co.us` host 403s automated requests, so never build that one.
const SOS_DETAIL: &str = "https://www.coloradosos.gov/biz/BusinessEntityDetail.do";
/// Where a human starts a fresh search on the official site.
const SOS_SEARCH: &str = "https://www.coloradosos.gov/biz/BusinessEntityCriteriaExt.do";

/// Enough to read; more than this and you wanted a different query.
const MAX_LIMIT: usize = 50;
/// Relevance cannot be expressed in SoQL — `like` has no ranking — so a name
/// search pulls a window and ranks it here. 200 comfortably covers a real
/// company: "STATE FARM" matches 37 rows statewide.
const RANK_WINDOW: usize = 200;

/// The columns this module maps. Naming them explicitly keeps the payload
/// small and documents exactly which upstream fields the mapping depends on,
/// so a schema change shows up as a missing field rather than a silent null.
const SELECT: &str = "entityid,entityname,entitystatus,entitytype,entityformdate,\
jurisdictonofformation,principaladdress1,principaladdress2,principalcity,principalstate,\
principalzipcode,principalcountry,agentfirstname,agentmiddlename,agentlastname,agentsuffix,\
agentorganizationname,agentprincipaladdress1,agentprincipaladdress2,agentprincipalcity,\
agentprincipalstate,agentprincipalzipcode,agentprincipalcountry,agentmailingaddress1,\
agentmailingaddress2,agentmailingcity,agentmailingstate,agentmailingzipcode,agentmailingcountry";

/// The only two statuses that mean the entity is alive and can be served in
/// the ordinary way. Everything else in the register — Delinquent,
/// Voluntarily/Judicially/Administratively Dissolved, Revoked, Withdrawn,
/// Merged, Converted, Noncompliant, "Registered Agent Resigned" — is a
/// service problem you want to know about before the process server goes out.
const ACTIVE_STATUSES: &[&str] = &["good standing", "exists"];

/// Every status value the dataset actually uses, verified by grouping the live
/// register. Quoted back at a caller whose `--status` filter matched nothing,
/// so they can fix the spelling instead of concluding the entity doesn't exist.
const KNOWN_STATUSES: &[&str] = &[
    "Administratively Dissolved",
    "Consolidated",
    "Converted",
    "Delinquent",
    "Dissolved (Term Expired)",
    "Effectiveness Prevented",
    "Exists",
    "Good Standing",
    "Judicially Dissolved",
    "Merged",
    "Noncompliant",
    "Registered Agent Resigned",
    "Revoked",
    "Voluntarily Dissolved",
    "Withdrawn",
];

pub(super) async fn fetch_entity(req: EntityRequest) -> Result<EntityResponse> {
    let plan = plan_query(&req)?;

    let mut out = EntityResponse {
        generated_at: chrono::Utc::now(),
        state: "co".to_string(),
        returned: 0,
        entities: Vec::new(),
        source: Some(
            "Colorado Secretary of State business entity register (data.colorado.gov mirror, \
             dataset 4ykn-tg5h)"
                .to_string(),
        ),
        source_url: Some(DATASET_PAGE.to_string()),
        warnings: plan.warnings.clone(),
    };

    // The snapshot caveat is not conditional on anything going wrong — it is
    // true of every answer this module gives, so it is always said.
    out.warnings.push(format!(
        "this is the daily Socrata mirror of the SOS register, not the register itself; before \
         serving anyone, confirm the agent and status on the official record at {SOS_SEARCH}"
    ));

    let Some(rows) = fetch_rows(&plan.params, &mut out.warnings).await else {
        return Ok(out);
    };

    if rows.len() >= plan.window && plan.needle.is_some() {
        out.warnings.push(format!(
            "the name search filled its {} row window, so the ranking below is over a partial \
             match set; narrow the name to be sure you are seeing the right entity",
            plan.window
        ));
    }

    let mut records: Vec<EntityRecord> = rows.iter().map(map_record).collect();
    if let Some(needle) = plan.needle.as_deref() {
        rank_by_name(&mut records, needle);
    }
    let matched = records.len();
    records.truncate(req.limit.clamp(1, MAX_LIMIT));
    if matched > records.len() {
        // Say how many were dropped. "Only one State Farm entity came back" is
        // a very different thing to act on than "one of nineteen".
        out.warnings.push(format!(
            "{matched} entities matched; showing the top {} by relevance, then good standing,              then formation date — raise the limit or narrow the name to see the rest",
            records.len()
        ));
    }

    if records.is_empty() {
        out.warnings.push(empty_result_note(&req));
    }
    flag_service_hazards(&records, &mut out.warnings);

    out.returned = records.len();
    out.entities = records;
    Ok(out)
}

// ── query planning ─────────────────────────────────────────────────────────

/// A fully-formed Socrata request plus what the caller needs to know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    params: Vec<(String, String)>,
    /// The lowercased name being matched, when this is a name search. `None`
    /// for an id lookup — there is nothing to rank one exact row against.
    needle: Option<String>,
    window: usize,
    warnings: Vec<String>,
}

/// Turn the request into SoQL.
///
/// `$q` is deliberately not used: it is Socrata's fuzzy full-text index, and it
/// happily returns "STATE FARM ROAD LLC" for a query aimed at the insurer. A
/// `$where ... like` is exact about what it matched, which is the property that
/// matters when the output is going on a return of service.
fn plan_query(req: &EntityRequest) -> Result<Plan> {
    let name = req.name.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let id = req
        .entity_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut warnings = Vec::new();
    let limit = req.limit.clamp(1, MAX_LIMIT);
    if req.limit > MAX_LIMIT {
        warnings.push(format!(
            "limit {} lowered to {MAX_LIMIT}; ask by name or id rather than paging the register",
            req.limit
        ));
    }

    let (clause, needle, window) = match (id, name) {
        (Some(id), name) => {
            if name.is_some() {
                warnings.push(
                    "both an entity id and a name were given; the id is exact, so the name was \
                     ignored"
                        .to_string(),
                );
            }
            // entityid is a Socrata number column, but it compares fine as a
            // quoted string and quoting keeps a typo'd id from becoming a SoQL
            // parse error the caller has to decode.
            (format!("entityid = '{}'", soql_escape(id)), None, limit)
        }
        (None, Some(name)) => (
            // upper() on both sides is the documented case-insensitive form;
            // SoQL `like` is case-sensitive on its own, and the register mixes
            // "State Farm" and "STATE FARM" freely.
            format!("upper(entityname) like upper('%{}%')", soql_escape(name)),
            Some(name.to_ascii_lowercase()),
            RANK_WINDOW.max(limit),
        ),
        (None, None) => {
            return Err(Error::InvalidInput(
                "a Colorado entity lookup needs something to look up: pass a company name \
                 (substring, case-insensitive — \"state farm mutual\") or an exact SOS entity id \
                 (\"19871032828\"). The id is on any SOS filing and is the only unambiguous key."
                    .to_string(),
            ));
        }
    };

    let mut where_clause = clause;
    if let Some(status) = req
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        // Matched as a substring so "dissolved" reaches all three flavors of
        // dissolution rather than silently matching none of them.
        where_clause.push_str(&format!(
            " AND upper(entitystatus) like upper('%{}%')",
            soql_escape(status)
        ));
    }

    let params = vec![
        ("$select".to_string(), SELECT.to_string()),
        ("$where".to_string(), where_clause),
        ("$limit".to_string(), window.to_string()),
        // Newest first is the right default for the wire: an insurer's current
        // Colorado registration is usually the recent one, and it gives the
        // local ranking a stable, deterministic input.
        ("$order".to_string(), "entityformdate DESC".to_string()),
    ];

    Ok(Plan {
        params,
        needle,
        window,
        warnings,
    })
}

/// SoQL string literals are single-quoted and escape a quote by doubling it.
///
/// This is not cosmetic. Colorado is full of names like "708 O'Brien Owners'
/// Association"; passing one through raw produces a 400 from the query
/// compiler, which without this looks exactly like "no such company".
fn soql_escape(input: &str) -> String {
    input.replace('\'', "''")
}

// ── transport ──────────────────────────────────────────────────────────────

async fn fetch_rows(
    params: &[(String, String)],
    warnings: &mut Vec<String>,
) -> Option<Vec<serde_json::Value>> {
    let resp = match shared_client::GENERAL
        .get(SOCRATA)
        .query(params)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("colorado entity search request failed: {e}"));
            return None;
        }
    };

    // Socrata answers a bad SoQL query with 400 and a JSON body naming the
    // problem. Left to the generic handler that becomes an empty result, which
    // reads as "this company is not registered in Colorado" — the most
    // dangerous wrong answer this tool could give.
    if resp.status() == reqwest::StatusCode::BAD_REQUEST {
        let body = resp.text().await.unwrap_or_default();
        warnings.push(format!(
            "colorado entity search: the Socrata query was rejected, so this is NOT a finding that \
             the entity is unregistered — {}",
            socrata_error_message(&body)
        ));
        return None;
    }

    let resp = soft_fail("colorado entity search", resp, warnings).await?;
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            warnings.push(format!("colorado entity search body read failed: {e}"));
            return None;
        }
    };
    match serde_json::from_str::<Vec<serde_json::Value>>(&body) {
        Ok(rows) => Some(rows),
        Err(e) => {
            warnings.push(format!(
                "colorado entity search returned something that is not a JSON array of rows ({e}); \
                 treating it as no answer rather than as no results"
            ));
            None
        }
    }
}

/// Pull the human sentence out of a Socrata error body, falling back to the
/// raw text so a caller is never told merely "it failed".
fn socrata_error_message(body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let msg = parsed
        .as_ref()
        .and_then(|v| v.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| body.trim().to_string());
    msg.chars().take(300).collect()
}

// ── mapping ────────────────────────────────────────────────────────────────

fn map_record(row: &serde_json::Value) -> EntityRecord {
    let id = field(row, "entityid");
    EntityRecord {
        url: id.as_deref().map(detail_url),
        entity_id: id,
        entity_name: field(row, "entityname"),
        status: field(row, "entitystatus"),
        entity_type: field(row, "entitytype"),
        formation_date: field(row, "entityformdate").map(|d| iso_date(&d)),
        jurisdiction: field(row, "jurisdictonofformation"),
        principal_address: join_address(row, "principal"),
        agent_name: agent_name(row),
        agent_address: agent_address(row),
    }
}

/// Socrata serializes its `number` columns as JSON strings *most* of the time
/// and as bare numbers occasionally, so read both rather than betting on one.
fn field(row: &serde_json::Value, key: &str) -> Option<String> {
    let v = row.get(key)?;
    let s = match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

/// `entityformdate` arrives as "1927-05-19T00:00:00.000". The time is always
/// midnight and always meaningless; a date is what a person reads.
fn iso_date(raw: &str) -> String {
    raw.split('T').next().unwrap_or(raw).to_string()
}

/// The register splits an address across six columns with a shared prefix
/// ("principal", "agentprincipal", "agentmailing"), so one helper serves all of
/// them.
fn join_address(row: &serde_json::Value, prefix: &str) -> Option<String> {
    let line1 = field(row, &format!("{prefix}address1"));
    let line2 = field(row, &format!("{prefix}address2"));
    let city = field(row, &format!("{prefix}city"));
    let state = field(row, &format!("{prefix}state"));
    let zip = field(row, &format!("{prefix}zipcode"));
    let country = field(row, &format!("{prefix}country"));

    let street = [line1, line2]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
    let locality = match (state, zip) {
        (Some(s), Some(z)) => Some(format!("{s} {z}")),
        (Some(s), None) => Some(s),
        (None, Some(z)) => Some(z),
        (None, None) => None,
    };
    // "US" on every domestic row is noise; a non-US country is not.
    let country = country.filter(|c| !c.eq_ignore_ascii_case("US"));

    let parts: Vec<String> = [
        (!street.is_empty()).then_some(street),
        city,
        locality,
        country,
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// An agent is either an organization (a commercial agent like Corporation
/// Service Company) or a natural person, and the register uses different
/// columns for each. The organization name wins when present because that is
/// the entity actually authorized to accept service.
fn agent_name(row: &serde_json::Value) -> Option<String> {
    if let Some(org) = field(row, "agentorganizationname") {
        return Some(org);
    }
    let parts: Vec<String> = [
        "agentfirstname",
        "agentmiddlename",
        "agentlastname",
        "agentsuffix",
    ]
    .iter()
    .filter_map(|k| field(row, k))
    .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The agent's *principal* address is the one to serve: Colorado requires a
/// registered agent to keep a street address in the state, and C.R.S.
/// 7-90-701(1) does not let it be a post office box. The mailing address may be
/// a box, so it is only a labelled fallback — never a silent substitute.
fn agent_address(row: &serde_json::Value) -> Option<String> {
    if let Some(addr) = join_address(row, "agentprincipal") {
        return Some(addr);
    }
    join_address(row, "agentmailing").map(|a| format!("{a} (agent mailing address — no street address of record; a PO box cannot be personally served)"))
}

fn detail_url(entity_id: &str) -> String {
    // The SOS detail page wants the same id in three parameters; omitting any
    // one of them bounces you back to the search form.
    format!(
        "{SOS_DETAIL}?quitButtonDestination=BusinessEntityResults&nameTyp=ENT&srchTyp=ENTITY\
         &masterFileId={id}&entityId2={id}&fileId={id}",
        id = urlencoding::encode(entity_id)
    )
}

// ── ranking and honesty ────────────────────────────────────────────────────

/// Newest-first is the wire order; this is the "best" half of "newest/best".
///
/// Name relevance dominates, because a caller who typed a full company name
/// wants that company even if it is dead (and will be warned that it is).
/// Among equally-good name matches, a live entity outranks a dissolved one,
/// and then newer outranks older. Without this, "state farm" answers with
/// whatever agency LLC registered most recently and buries STATE FARM MUTUAL
/// AUTOMOBILE INSURANCE COMPANY — formed in 1927 — at the bottom.
fn rank_by_name(records: &mut [EntityRecord], needle_lower: &str) {
    records.sort_by(|a, b| rank_key(a, needle_lower).cmp(&rank_key(b, needle_lower)));
}

fn rank_key(rec: &EntityRecord, needle_lower: &str) -> (u8, u8, std::cmp::Reverse<String>) {
    let name = rec
        .entity_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let core = core_name(&name);
    let name_tier = if core == needle_lower || name == needle_lower {
        0
    } else if core.starts_with(needle_lower) || name.starts_with(needle_lower) {
        1
    } else if name.contains(needle_lower) {
        2
    } else {
        3
    };
    let status_tier = u8::from(!is_active(rec.status.as_deref()));
    (
        name_tier,
        status_tier,
        std::cmp::Reverse(rec.formation_date.clone().unwrap_or_default()),
    )
}

/// The register appends the delinquency to the *name*: "6301 State Farm LLC,
/// Delinquent May 1, 2022". Searching the company's actual name would then
/// never score as an exact match, so the suffix is stripped for comparison
/// only — it is left intact in the record, because it is what the SOS shows.
fn core_name(name_lower: &str) -> &str {
    for marker in [
        ", delinquent ",
        ", dissolved ",
        ", revoked ",
        ", withdrawn ",
    ] {
        if let Some(i) = name_lower.find(marker) {
            return name_lower[..i].trim_end();
        }
    }
    name_lower
}

fn is_active(status: Option<&str>) -> bool {
    let Some(s) = status else { return false };
    let s = s.trim().to_ascii_lowercase();
    ACTIVE_STATUSES.iter().any(|a| s == *a)
}

/// Say plainly what would go wrong. A dissolved entity, a delinquent one, or
/// one whose agent has resigned each break service in a different way, and
/// none of them is visible from the name.
fn flag_service_hazards(records: &[EntityRecord], warnings: &mut Vec<String>) {
    let dead: Vec<String> = records
        .iter()
        .filter(|r| !is_active(r.status.as_deref()))
        .map(|r| {
            format!(
                "{} [{}]",
                r.entity_name.as_deref().unwrap_or("(unnamed)"),
                r.status.as_deref().unwrap_or("status not stated")
            )
        })
        .collect();
    if !dead.is_empty() {
        warnings.push(format!(
            "{} of {} results are NOT in good standing: {}. Serving a dissolved, delinquent or \
             revoked entity at its old agent may not be effective service — check whether the \
             entity was reinstated, and consider service on the Secretary of State under C.R.S. \
             7-90-704(2).",
            dead.len(),
            records.len(),
            dead.join("; ")
        ));
    }

    let agentless: Vec<String> = records
        .iter()
        .filter(|r| r.agent_name.is_none())
        .filter_map(|r| r.entity_name.clone())
        .collect();
    if !agentless.is_empty() {
        warnings.push(format!(
            "no registered agent of record in this snapshot for: {}. A C.R.S. 10-3-1117 demand has \
             no one to serve, and C.R.S. 7-90-704(2) service on the Secretary of State is the \
             usual route — verify on the official record first.",
            agentless.join("; ")
        ));
    }
}

fn empty_result_note(req: &EntityRequest) -> String {
    let mut note = if req.entity_id.is_some() {
        "no Colorado entity carries that id. SOS entity ids are 11 digits and begin with the \
         filing year (e.g. 19871032828); confirm it against a filing."
            .to_string()
    } else {
        "no Colorado entity name contains that substring. Insurers often register under a longer \
         formal name than the brand — try a distinctive fragment (\"state farm\" rather than \
         \"State Farm Insurance\")."
            .to_string()
    };
    if let Some(status) = req.status.as_deref().filter(|s| !s.trim().is_empty()) {
        note.push_str(&format!(
            " The status filter {status:?} also had to match; the register uses exactly these \
             values: {}.",
            KNOWN_STATUSES.join(", ")
        ));
    }
    note
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> EntityRequest {
        EntityRequest {
            state: "co".to_string(),
            name: None,
            entity_id: None,
            status: None,
            limit: 10,
        }
    }

    fn param<'a>(plan: &'a Plan, key: &str) -> &'a str {
        plan.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or_default()
    }

    /// A real, trimmed response: the STATE FARM name search, cut to the rows
    /// that exercise the mapping — a commercial agent, a natural-person agent,
    /// and a delinquent entity whose status is baked into its name.
    const FIXTURE: &str = r#"[
      {"entityid":"19871032828","entityname":"STATE FARM MUTUAL AUTOMOBILE INSURANCE COMPANY",
       "principaladdress1":"1 State Farm Plz","principalcity":"Bloomington","principalstate":"IL",
       "principalzipcode":"61710","principalcountry":"US","entitystatus":"Good Standing",
       "jurisdictonofformation":"IL","entitytype":"FPC","agentorganizationname":"CORPORATION SERVICE COMPANY",
       "agentprincipaladdress1":"1900 W Littleton Blvd","agentprincipalcity":"Littleton",
       "agentprincipalstate":"CO","agentprincipalzipcode":"80120","agentprincipalcountry":"US",
       "entityformdate":"1927-05-19T00:00:00.000"},
      {"entityid":"20218092499","entityname":"Kaitlin Bagley State Farm LLC",
       "principaladdress1":"5001 Ward Rd","principalcity":"Wheat Ridge","principalstate":"CO",
       "principalzipcode":"80033","principalcountry":"US","entitystatus":"Good Standing",
       "jurisdictonofformation":"CO","entitytype":"DLLC","agentfirstname":"KAITLIN",
       "agentmiddlename":"CHRISTINE","agentlastname":"BAGLEY","agentprincipaladdress1":"15520 W 48th Ave",
       "agentprincipalcity":"Golden","agentprincipalstate":"CO","agentprincipalzipcode":"80403",
       "agentprincipalcountry":"US","entityformdate":"2021-11-22T00:00:00.000"},
      {"entityid":"20208090445","entityname":"6301 State Farm LLC, Delinquent May 1, 2022",
       "principaladdress1":"55 Madison Street, Ste 625","principalcity":"Denver","principalstate":"CO",
       "principalzipcode":"80206","principalcountry":"US","entitystatus":"Delinquent",
       "jurisdictonofformation":"CO","entitytype":"DLLC","agentorganizationname":"Accruit, LLC",
       "agentprincipaladdress1":"55 Madison Street, Ste 625","agentprincipalcity":"Denver",
       "agentprincipalstate":"CO","agentprincipalzipcode":"80206","agentprincipalcountry":"US",
       "entityformdate":"2020-12-21T00:00:00.000"}
    ]"#;

    fn fixture_records() -> Vec<EntityRecord> {
        serde_json::from_str::<Vec<serde_json::Value>>(FIXTURE)
            .expect("fixture parses")
            .iter()
            .map(map_record)
            .collect()
    }

    #[test]
    fn name_and_id_dispatch_to_different_clauses() {
        let by_name = plan_query(&EntityRequest {
            name: Some("state farm".to_string()),
            ..req()
        })
        .expect("name plan");
        assert_eq!(
            param(&by_name, "$where"),
            "upper(entityname) like upper('%state farm%')"
        );
        assert_eq!(by_name.needle.as_deref(), Some("state farm"));

        let by_id = plan_query(&EntityRequest {
            entity_id: Some("19871032828".to_string()),
            ..req()
        })
        .expect("id plan");
        assert_eq!(param(&by_id, "$where"), "entityid = '19871032828'");
        // An exact row has nothing to rank, and ranking it would be a lie.
        assert!(by_id.needle.is_none());
    }

    /// The bug this prevents: an unescaped apostrophe makes Socrata 400, which
    /// without the escape reads back to a lawyer as "no such company".
    #[test]
    fn escapes_single_quotes_in_a_company_name() {
        assert_eq!(soql_escape("O'Brien"), "O''Brien");
        assert_eq!(
            soql_escape("708 O'Brien Owners' Association"),
            "708 O''Brien Owners'' Association"
        );
        let plan = plan_query(&EntityRequest {
            name: Some("O'Brien".to_string()),
            ..req()
        })
        .expect("plan");
        assert_eq!(
            param(&plan, "$where"),
            "upper(entityname) like upper('%O''Brien%')"
        );
    }

    #[test]
    fn status_filter_is_anded_onto_the_clause() {
        let plan = plan_query(&EntityRequest {
            name: Some("acme".to_string()),
            status: Some("dissolved".to_string()),
            ..req()
        })
        .expect("plan");
        assert_eq!(
            param(&plan, "$where"),
            "upper(entityname) like upper('%acme%') AND upper(entitystatus) like upper('%dissolved%')"
        );
    }

    #[test]
    fn id_wins_over_name_and_says_so() {
        let plan = plan_query(&EntityRequest {
            name: Some("state farm".to_string()),
            entity_id: Some("19871032828".to_string()),
            ..req()
        })
        .expect("plan");
        assert_eq!(param(&plan, "$where"), "entityid = '19871032828'");
        assert!(
            plan.warnings.iter().any(|w| w.contains("name was ignored")),
            "{:?}",
            plan.warnings
        );
    }

    #[test]
    fn neither_name_nor_id_is_an_input_error() {
        let e = plan_query(&req()).expect_err("must fail");
        assert!(matches!(e, Error::InvalidInput(_)), "{e:?}");
        let msg = e.to_string();
        assert!(msg.contains("company name"), "{msg}");
        assert!(msg.contains("entity id"), "{msg}");

        // Whitespace-only is the same as absent, not a search for " ".
        let blank = plan_query(&EntityRequest {
            name: Some("   ".to_string()),
            entity_id: Some("".to_string()),
            ..req()
        });
        assert!(blank.is_err());
    }

    #[test]
    fn limit_is_capped_and_the_window_is_wider_than_the_answer() {
        let plan = plan_query(&EntityRequest {
            name: Some("acme".to_string()),
            limit: 5_000,
            ..req()
        })
        .expect("plan");
        assert_eq!(param(&plan, "$limit"), RANK_WINDOW.to_string());
        assert!(
            plan.warnings.iter().any(|w| w.contains("lowered to")),
            "{:?}",
            plan.warnings
        );

        // An id lookup does not need the ranking window.
        let by_id = plan_query(&EntityRequest {
            entity_id: Some("19871032828".to_string()),
            limit: 3,
            ..req()
        })
        .expect("plan");
        assert_eq!(param(&by_id, "$limit"), "3");
    }

    #[test]
    fn maps_a_real_row_into_the_record_shape() {
        let recs = fixture_records();
        let sf = &recs[0];
        assert_eq!(sf.entity_id.as_deref(), Some("19871032828"));
        assert_eq!(
            sf.entity_name.as_deref(),
            Some("STATE FARM MUTUAL AUTOMOBILE INSURANCE COMPANY")
        );
        assert_eq!(sf.status.as_deref(), Some("Good Standing"));
        assert_eq!(sf.entity_type.as_deref(), Some("FPC"));
        assert_eq!(sf.jurisdiction.as_deref(), Some("IL"));
        // Date, not a midnight timestamp.
        assert_eq!(sf.formation_date.as_deref(), Some("1927-05-19"));
        assert_eq!(
            sf.principal_address.as_deref(),
            Some("1 State Farm Plz, Bloomington, IL 61710")
        );
        // The whole point of the module: agent name and a servable CO street address.
        assert_eq!(
            sf.agent_name.as_deref(),
            Some("CORPORATION SERVICE COMPANY")
        );
        assert_eq!(
            sf.agent_address.as_deref(),
            Some("1900 W Littleton Blvd, Littleton, CO 80120")
        );
    }

    #[test]
    fn builds_a_natural_person_agent_name_from_the_split_columns() {
        let recs = fixture_records();
        assert_eq!(
            recs[1].agent_name.as_deref(),
            Some("KAITLIN CHRISTINE BAGLEY")
        );
    }

    #[test]
    fn status_passes_through_verbatim_and_is_flagged() {
        let recs = fixture_records();
        assert_eq!(recs[2].status.as_deref(), Some("Delinquent"));

        let mut warnings = Vec::new();
        flag_service_hazards(&recs, &mut warnings);
        let joined = warnings.join(" | ");
        assert!(joined.contains("NOT in good standing"), "{joined}");
        assert!(joined.contains("6301 State Farm LLC"), "{joined}");
        assert!(joined.contains("[Delinquent]"), "{joined}");
        assert!(joined.contains("7-90-704(2)"), "{joined}");
        // The two good-standing rows must not be accused.
        assert!(!joined.contains("MUTUAL AUTOMOBILE"), "{joined}");
    }

    #[test]
    fn missing_agent_is_called_out_not_left_blank() {
        let row: serde_json::Value = serde_json::from_str(
            r#"{"entityid":"1","entityname":"NO AGENT LLC","entitystatus":"Registered Agent Resigned"}"#,
        )
        .expect("row");
        let rec = map_record(&row);
        assert!(rec.agent_name.is_none());
        assert!(rec.agent_address.is_none());

        let mut warnings = Vec::new();
        flag_service_hazards(std::slice::from_ref(&rec), &mut warnings);
        assert!(
            warnings.iter().any(|w| w.contains("10-3-1117")),
            "{warnings:?}"
        );
    }

    #[test]
    fn ranks_the_insurer_above_the_newer_agency_llc() {
        let mut recs = fixture_records();
        // Wire order is newest-first, so the 1927 insurer arrives last.
        assert_eq!(recs[0].entity_id.as_deref(), Some("19871032828"));
        recs.reverse();
        rank_by_name(&mut recs, "state farm");
        assert_eq!(
            recs[0].entity_name.as_deref(),
            Some("STATE FARM MUTUAL AUTOMOBILE INSURANCE COMPANY"),
            "prefix match in good standing must come first"
        );
        // The delinquent one sinks below the live ones.
        assert_eq!(recs[2].status.as_deref(), Some("Delinquent"));
    }

    /// The register writes the delinquency into the name, so an exact-name
    /// search would otherwise never score as exact.
    #[test]
    fn exact_match_survives_the_delinquency_suffix_in_the_name() {
        assert_eq!(
            core_name("6301 state farm llc, delinquent may 1, 2022"),
            "6301 state farm llc"
        );
        let recs = fixture_records();
        let key = rank_key(&recs[2], "6301 state farm llc");
        assert_eq!(key.0, 0, "should be an exact name match");
        assert_eq!(key.1, 1, "but still marked not-active");
    }

    #[test]
    fn active_means_good_standing_or_exists_and_nothing_else() {
        assert!(is_active(Some("Good Standing")));
        assert!(is_active(Some("exists")));
        for dead in [
            "Delinquent",
            "Voluntarily Dissolved",
            "Withdrawn",
            "Registered Agent Resigned",
        ] {
            assert!(!is_active(Some(dead)), "{dead}");
        }
        assert!(!is_active(None));
    }

    #[test]
    fn builds_an_openable_sos_detail_url() {
        let url = detail_url("19871032828");
        assert!(
            url.starts_with("https://www.coloradosos.gov/biz/BusinessEntityDetail.do?"),
            "{url}"
        );
        // All three id parameters are required by the SOS page.
        for p in [
            "masterFileId=19871032828",
            "entityId2=19871032828",
            "fileId=19871032828",
        ] {
            assert!(url.contains(p), "missing {p} in {url}");
        }
        // Never the sos.state.co.us host — it 403s.
        assert!(!url.contains("sos.state.co.us"), "{url}");
        assert_eq!(fixture_records()[0].url.as_deref(), Some(url.as_str()));
    }

    #[test]
    fn foreign_country_is_kept_but_us_is_dropped_as_noise() {
        let dom: serde_json::Value = serde_json::from_str(
            r#"{"principaladdress1":"1 Main","principalcity":"Denver","principalstate":"CO","principalzipcode":"80202","principalcountry":"US"}"#,
        )
        .expect("row");
        assert_eq!(
            join_address(&dom, "principal").as_deref(),
            Some("1 Main, Denver, CO 80202")
        );
        let intl: serde_json::Value = serde_json::from_str(
            r#"{"principaladdress1":"1 King St","principalcity":"Toronto","principalstate":"ON","principalzipcode":"M5H","principalcountry":"CA"}"#,
        )
        .expect("row");
        assert_eq!(
            join_address(&intl, "principal").as_deref(),
            Some("1 King St, Toronto, ON M5H, CA")
        );
        assert!(join_address(&serde_json::json!({}), "principal").is_none());
    }

    /// A 400 must never be reported as "no results" — that reads as proof the
    /// company is not registered.
    #[test]
    fn socrata_error_body_is_turned_into_a_readable_message() {
        let body = r#"{"code":"query.compiler.malformed","error":true,"message":"Could not parse SoQL query at line 1 character 49"}"#;
        assert!(socrata_error_message(body).contains("Could not parse SoQL query"));
        // A non-JSON body still yields something.
        assert_eq!(
            socrata_error_message("  gateway timeout "),
            "gateway timeout"
        );
    }

    #[test]
    fn empty_result_note_names_the_real_status_values() {
        let note = empty_result_note(&EntityRequest {
            name: Some("acme".to_string()),
            status: Some("activ".to_string()),
            ..req()
        });
        assert!(note.contains("Good Standing"), "{note}");
        assert!(note.contains("Voluntarily Dissolved"), "{note}");
        let id_note = empty_result_note(&EntityRequest {
            entity_id: Some("123".to_string()),
            ..req()
        });
        assert!(id_note.contains("11 digits"), "{id_note}");
    }

    // ── live checks ────────────────────────────────────────────────────────
    //
    // Ignored by default, exactly like the Wisconsin and Massachusetts live
    // tests: `cargo test -p eli-core legal::` touches no network. Run them by
    // name when the upstream schema is in question.

    #[tokio::test]
    #[ignore = "hits data.colorado.gov"]
    async fn live_state_farm_has_a_servable_registered_agent() {
        let out = fetch_entity(EntityRequest {
            name: Some("state farm mutual automobile".to_string()),
            ..req()
        })
        .await
        .expect("lookup");
        let first = out.entities.first().expect("at least one match");
        assert_eq!(
            first.entity_name.as_deref(),
            Some("STATE FARM MUTUAL AUTOMOBILE INSURANCE COMPANY")
        );
        assert_eq!(first.status.as_deref(), Some("Good Standing"));
        let agent = first.agent_name.as_deref().expect("registered agent name");
        let addr = first
            .agent_address
            .as_deref()
            .expect("registered agent address");
        assert!(!agent.is_empty(), "agent name is the whole point");
        assert!(
            addr.contains("CO"),
            "agent must have a Colorado address: {addr}"
        );
        assert!(out
            .warnings
            .iter()
            .any(|w| w.contains("daily Socrata mirror")));
    }

    #[tokio::test]
    #[ignore = "hits data.colorado.gov"]
    async fn live_exact_id_lookup_round_trips() {
        let out = fetch_entity(EntityRequest {
            entity_id: Some("19871032828".to_string()),
            ..req()
        })
        .await
        .expect("lookup");
        assert_eq!(out.returned, 1);
        assert_eq!(
            out.entities[0].formation_date.as_deref(),
            Some("1927-05-19")
        );
    }

    /// Socrata sometimes hands back a number where it usually hands back a
    /// string; both must map, and an empty string is absence, not a value.
    #[test]
    fn reads_numbers_and_strings_and_treats_blank_as_absent() {
        let row = serde_json::json!({"a": 19871032828i64, "b": " x ", "c": "", "d": null});
        assert_eq!(field(&row, "a").as_deref(), Some("19871032828"));
        assert_eq!(field(&row, "b").as_deref(), Some("x"));
        assert!(field(&row, "c").is_none());
        assert!(field(&row, "d").is_none());
        assert!(field(&row, "missing").is_none());
    }
}
