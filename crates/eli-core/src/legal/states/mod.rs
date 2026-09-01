//! State-law sources — the layers CourtListener and the federal tools don't reach.
//!
//! Three capabilities live here, each chosen because it answers something no
//! search engine and no federal source can:
//!
//!   * **State statutes.** The text of the law you were actually charged under.
//!     There is no all-50-state statute API — Open States and LegiScan carry
//!     bills, not codified code — so this is necessarily per-state.
//!   * **Trial-court case records.** The rarest layer by far. Traffic, small
//!     claims and misdemeanors are decided in trial courts, which publish
//!     nothing and sit behind PACER-style paywalls or bot walls in nearly every
//!     state. Wisconsin is the exception and the reason this module exists.
//!   * **Unpublished appellate opinions.** CourtListener's search returns
//!     Published opinions by default, but a state's own feed often carries the
//!     non-precedential orders — which is most of what a traffic or small-claims
//!     appeal actually produces.
//!
//! Coverage is deliberately narrow and honest: a state is listed only when a
//! layer has been verified against the live source. `supported()` is the
//! machine-readable truth, and every response says which source answered.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub mod colorado;
pub mod colorado_damages;
pub mod colorado_deadlines;
pub mod colorado_entity;
pub mod massachusetts;
pub mod opinions;
pub mod wisconsin;

/// What a given state can actually answer. Kept as data rather than prose so
/// the MCP layer can tell a model "not covered" before it spends a call.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateCoverage {
    /// Two-letter code, lowercase.
    pub code: &'static str,
    pub name: &'static str,
    /// Statutory text by section.
    pub statutes: bool,
    /// Trial-court case records — the layer where a traffic ticket lives.
    pub trial_records: bool,
    /// Appellate opinions feed, including non-precedential ones.
    pub opinions: bool,
    /// Business-entity and registered-agent register.
    pub entities: bool,
    /// Damages caps resolvable by date.
    pub damages_caps: bool,
    /// Case deadlines computable from statute.
    pub deadlines: bool,
}

/// The verified coverage table. Adding a row here without a working
/// implementation behind it is the one thing that would make this module lie,
/// so the CLI tests assert the table against the dispatchers.
pub const COVERAGE: &[StateCoverage] = &[
    StateCoverage {
        code: "wi",
        name: "Wisconsin",
        statutes: true,
        trial_records: true,
        opinions: false,
            entities: false,
        damages_caps: false,
        deadlines: false,
    },
    StateCoverage {
        code: "co",
        name: "Colorado",
        statutes: true,
        trial_records: false,
        opinions: false,
            entities: true,
        damages_caps: true,
        deadlines: true,
    },
    StateCoverage {
        code: "ma",
        name: "Massachusetts",
        statutes: true,
        trial_records: false,
        opinions: false,
            entities: false,
        damages_caps: false,
        deadlines: false,
    },
    StateCoverage {
        code: "il",
        name: "Illinois",
        statutes: false,
        trial_records: false,
        opinions: true,
            entities: false,
        damages_caps: false,
        deadlines: false,
    },
    StateCoverage {
        code: "pa",
        name: "Pennsylvania",
        statutes: false,
        trial_records: false,
        opinions: true,
            entities: false,
        damages_caps: false,
        deadlines: false,
    },
    StateCoverage {
        code: "mi",
        name: "Michigan",
        statutes: false,
        trial_records: false,
        opinions: true,
            entities: false,
        damages_caps: false,
        deadlines: false,
    },
    StateCoverage {
        code: "nj",
        name: "New Jersey",
        statutes: false,
        trial_records: false,
        opinions: true,
            entities: false,
        damages_caps: false,
        deadlines: false,
    },
];

pub fn supported(code: &str) -> Option<&'static StateCoverage> {
    let c = normalize_state(code);
    COVERAGE.iter().find(|s| s.code == c)
}

/// Accept "wi", "WI", "Wisconsin" — a model will pass any of them.
pub fn normalize_state(input: &str) -> String {
    let raw = input.trim().to_ascii_lowercase();
    if raw.len() == 2 {
        return raw;
    }
    COVERAGE
        .iter()
        .find(|s| s.name.to_ascii_lowercase() == raw)
        .map(|s| s.code.to_string())
        .unwrap_or(raw)
}

/// Build the "not covered" error, naming what IS covered for that layer so the
/// caller can redirect instead of guessing again.
pub(crate) fn unsupported(code: &str, layer: &str, have: impl Fn(&StateCoverage) -> bool) -> Error {
    let covered: Vec<&str> = COVERAGE
        .iter()
        .filter(|s| have(s))
        .map(|s| s.code)
        .collect();
    Error::InvalidInput(format!(
        "no {layer} source for state {code:?}. Covered for {layer}: {}. \
         Coverage is per-state because no all-50-state source exists for this layer.",
        if covered.is_empty() {
            "none".to_string()
        } else {
            covered.join(", ")
        }
    ))
}

// ── statutes ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct StateStatuteRequest {
    pub state: String,
    /// Section as cited, e.g. "346.57" (WI) or "90/17" / "17" (MA chapter 90).
    pub section: String,
    /// MA addresses sections within a chapter; WI encodes the chapter in the
    /// section number. Optional so one request type serves both.
    pub chapter: Option<String>,
    pub max_chars: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateStatuteResponse {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub state: String,
    pub citation: Option<String>,
    pub heading: Option<String>,
    pub text: Option<String>,
    /// Length of the FULL section text, before `max_chars` clamping — so a
    /// caller can see how much it did not get. Reporting the clamped length
    /// here instead makes `chars == max_chars` and silently hides the rest.
    pub chars: usize,
    pub truncated: bool,
    /// True when the source says the section has been repealed — worth its own
    /// field, because quoting a repealed statute as current law is the same
    /// class of error as citing a case that does not exist.
    pub repealed: Option<bool>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_state_statute(req: StateStatuteRequest) -> Result<StateStatuteResponse> {
    let code = normalize_state(&req.state);
    match code.as_str() {
        "wi" => wisconsin::fetch_statute(req).await,
        "ma" => massachusetts::fetch_statute(req).await,
        "co" => colorado::fetch_statute(req).await,
        _ => Err(unsupported(&code, "statute", |s| s.statutes)),
    }
}

// ── trial-court records ────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct StateCaseRequest {
    pub state: String,
    /// County name or number. Wisconsin's API is county-scoped.
    pub county: Option<String>,
    /// Case-type code, e.g. "TR" (traffic), "SC" (small claims), "CV", "CM".
    pub case_type: Option<String>,
    /// Exact case number, e.g. "2024TR000321".
    pub case_no: Option<String>,
    pub filed_after: Option<String>,
    pub filed_before: Option<String>,
    pub limit: usize,
    /// Court records name private individuals. Dates of birth come back from
    /// Wisconsin's API and are almost never needed to answer a question, so
    /// they are redacted unless the caller explicitly opts in.
    pub include_dob: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct StateCaseRecord {
    pub case_no: Option<String>,
    pub caption: Option<String>,
    pub party_name: Option<String>,
    pub county: Option<String>,
    pub filing_date: Option<String>,
    pub status: Option<String>,
    /// Only populated when `include_dob` was set; otherwise omitted entirely.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dob: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateCaseResponse {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub state: String,
    pub returned: usize,
    pub cases: Vec<StateCaseRecord>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_state_cases(req: StateCaseRequest) -> Result<StateCaseResponse> {
    let code = normalize_state(&req.state);
    match code.as_str() {
        "wi" => wisconsin::fetch_cases(req).await,
        _ => Err(unsupported(&code, "trial-court record", |s| s.trial_records)),
    }
}

// ── appellate opinions (incl. unpublished) ─────────────────────────────────

#[derive(Clone, Debug)]
pub struct StateOpinionsRequest {
    pub state: String,
    /// Client-side keyword filter; most of these feeds have no query param.
    pub query: Option<String>,
    /// Return only non-precedential / unpublished items — the gap
    /// CourtListener's Published-default leaves.
    pub unpublished_only: bool,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct StateOpinion {
    pub case_name: Option<String>,
    pub court: Option<String>,
    pub filed: Option<String>,
    pub citation: Option<String>,
    pub docket: Option<String>,
    /// e.g. "Rule 23 Order", "Unpublished", "Opinion".
    pub disposition: Option<String>,
    pub published: Option<bool>,
    pub pdf_url: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateOpinionsResponse {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub state: String,
    pub returned: usize,
    /// How many of the returned items are non-precedential — the headline
    /// number, since that is what this adds over CourtListener.
    pub unpublished_count: usize,
    pub opinions: Vec<StateOpinion>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_state_opinions(req: StateOpinionsRequest) -> Result<StateOpinionsResponse> {
    let code = normalize_state(&req.state);
    match code.as_str() {
        "il" | "pa" | "mi" | "nj" => opinions::fetch(req).await,
        _ => Err(unsupported(&code, "opinions feed", |s| s.opinions)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_state_names_and_codes() {
        assert_eq!(normalize_state("WI"), "wi");
        assert_eq!(normalize_state(" Wisconsin "), "wi");
        assert_eq!(normalize_state("Massachusetts"), "ma");
        assert_eq!(normalize_state("zz"), "zz");
    }

    #[test]
    fn coverage_table_has_no_duplicate_codes() {
        let mut codes: Vec<&str> = COVERAGE.iter().map(|s| s.code).collect();
        codes.sort_unstable();
        let before = codes.len();
        codes.dedup();
        assert_eq!(before, codes.len(), "duplicate state code in COVERAGE");
    }

    #[test]
    fn unsupported_names_the_states_that_do_work() {
        let e = unsupported("zz", "statute", |s| s.statutes);
        let msg = e.to_string();
        assert!(msg.contains("wi") && msg.contains("ma"), "{msg}");
    }

    /// Every dispatcher arm must correspond to a COVERAGE row, or the table
    /// tells the model something the code cannot deliver.
    #[test]
    fn dispatchers_match_the_coverage_table() {
        for s in COVERAGE {
            if s.statutes {
                assert!(
                    matches!(s.code, "wi" | "ma" | "co"),
                    "statute arm missing for {}",
                    s.code
                );
            }
            if s.trial_records {
                assert!(matches!(s.code, "wi"), "trial-record arm missing for {}", s.code);
            }
            if s.opinions {
                assert!(
                    matches!(s.code, "il" | "pa" | "mi" | "nj"),
                    "opinions arm missing for {}",
                    s.code
                );
            }
            // The three Colorado-only layers. The table is what a caller reads
            // to know what is answerable, so a row claiming a layer with no
            // dispatcher behind it would be the table telling a lie.
            for (claimed, layer) in [
                (s.entities, "entity"),
                (s.damages_caps, "damages cap"),
                (s.deadlines, "deadline"),
            ] {
                if claimed {
                    assert_eq!(s.code, "co", "{layer} arm missing for {}", s.code);
                }
            }
        }
    }
}

// ── damages caps ───────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct DamagesCapRequest {
    pub state: String,
    /// noneconomic | wrongful_death | solatium | dram_shop | medmal
    pub claim_type: String,
    /// When the claim accrued — usually, but not always, the injury date.
    pub accrual_date: String,
    /// When suit was (or will be) filed. Colorado's 2025 amendment keys off
    /// BOTH dates, so omitting this can change the answer by a factor of two.
    pub filing_date: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct DamagesCap {
    pub claim_type: String,
    pub citation: String,
    pub amount: Option<i64>,
    /// Some caps may be raised by the court on clear and convincing evidence.
    pub increased_maximum: Option<i64>,
    /// Which accrual band or statutory provision produced this figure.
    pub basis: String,
    pub source: String,
    pub source_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DamagesCapResponse {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub state: String,
    pub accrual_date: String,
    pub filing_date: Option<String>,
    pub caps: Vec<DamagesCap>,
    /// The certificate revision this answer was built from, so a caller can
    /// tell whether it predates a newer certification.
    pub certificate_revised: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_damages_cap(req: DamagesCapRequest) -> Result<DamagesCapResponse> {
    let code = normalize_state(&req.state);
    match code.as_str() {
        "co" => colorado_damages::fetch(req).await,
        _ => Err(unsupported(&code, "damages cap", |s| s.code == "co")),
    }
}

// ── deadlines ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct DeadlinesRequest {
    pub state: String,
    /// Date of injury. Accrual can differ under the discovery rule — the
    /// response says so rather than pretending the two are always the same.
    pub injury_date: String,
    /// motor_vehicle | general | medical | premises | other
    pub mechanism: String,
    /// Whether any public entity or public employee is implicated. In Denver
    /// that means RTD, the City, and Denver Health among others.
    pub public_entity: bool,
    /// Whether a licensed professional is a defendant (drives the certificate
    /// of review).
    pub professional_defendant: bool,
    /// Optional known dates that unlock the post-filing deadlines.
    pub filing_date: Option<String>,
    pub service_date: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Deadline {
    pub name: String,
    pub citation: String,
    /// Computed date, when the inputs allow it.
    pub date: Option<String>,
    /// What the date is measured from, named explicitly.
    pub runs_from: String,
    pub period: String,
    /// True when blowing this ends the claim outright.
    pub fatal: bool,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeadlinesResponse {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub state: String,
    pub injury_date: String,
    pub deadlines: Vec<Deadline>,
    /// Determinations a person has to make; the tool computes dates, it does
    /// not decide whether a defendant is a public entity.
    pub judgment_calls: Vec<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_deadlines(req: DeadlinesRequest) -> Result<DeadlinesResponse> {
    let code = normalize_state(&req.state);
    match code.as_str() {
        "co" => colorado_deadlines::fetch(req).await,
        _ => Err(unsupported(&code, "deadline", |s| s.code == "co")),
    }
}

// ── business entities ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct EntityRequest {
    pub state: String,
    pub name: Option<String>,
    pub entity_id: Option<String>,
    pub status: Option<String>,
    pub limit: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct EntityRecord {
    pub entity_id: Option<String>,
    pub entity_name: Option<String>,
    pub status: Option<String>,
    pub entity_type: Option<String>,
    pub formation_date: Option<String>,
    pub jurisdiction: Option<String>,
    pub principal_address: Option<String>,
    pub agent_name: Option<String>,
    pub agent_address: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntityResponse {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub state: String,
    pub returned: usize,
    pub entities: Vec<EntityRecord>,
    pub source: Option<String>,
    pub source_url: Option<String>,
    pub warnings: Vec<String>,
}

pub async fn fetch_entity(req: EntityRequest) -> Result<EntityResponse> {
    let code = normalize_state(&req.state);
    match code.as_str() {
        "co" => colorado_entity::fetch_entity(req).await,
        _ => Err(unsupported(&code, "entity lookup", |s| s.code == "co")),
    }
}
