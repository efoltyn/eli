//! Colorado personal-injury deadlines, computed from the injury date.
//!
//! These are the rules that decide whether a claim exists at all, as opposed to
//! what it is worth, and they are the ones most often gotten wrong because
//! Colorado splits where other states do not: a general tort is two years, but a
//! tort arising from the use or operation of a motor vehicle is **three**. Same
//! injury, different clock, decided by a fact about the mechanism.
//!
//! The Governmental Immunity Act notice is worse: 182 days, jurisdictional, and
//! it applies to defendants people do not think of as governmental. In Denver
//! that includes RTD and Denver Health. Miss it and no amount of merit revives
//! the claim.
//!
//! WHAT THIS TOOL DOES AND DOES NOT DO. It computes dates from periods fixed by
//! statute and names the authority for each. It does NOT decide the legal
//! questions those periods hang on — whether a defendant is a public entity,
//! whether the discovery rule moved accrual off the injury date, whether a
//! vehicle was in "use or operation". Those are determinations, and the response
//! puts them in `judgment_calls` rather than silently assuming an answer.

use super::{Deadline, DeadlinesRequest, DeadlinesResponse};
use crate::legal::parse_date;
use crate::{Error, Result};
use chrono::{Duration, NaiveDate};

pub(super) async fn fetch(req: DeadlinesRequest) -> Result<DeadlinesResponse> {
    let injury = parse_date(&req.injury_date, "injury_date")?;
    let filing = match req.filing_date.as_deref() {
        Some(d) => Some(parse_date(d, "filing_date")?),
        None => None,
    };
    let service = match req.service_date.as_deref() {
        Some(d) => Some(parse_date(d, "service_date")?),
        None => None,
    };

    let mechanism = req.mechanism.trim().to_ascii_lowercase();
    let is_mv = matches!(
        mechanism.as_str(),
        "motor_vehicle" | "mv" | "auto" | "car" | "vehicle" | "truck" | "motorcycle"
    );
    if !is_mv
        && !matches!(
            mechanism.as_str(),
            "" | "general" | "medical" | "premises" | "other" | "slip_and_fall" | "product"
        )
    {
        return Err(Error::InvalidInput(format!(
            "unknown mechanism {:?}; use motor_vehicle, general, medical, premises, product, or other",
            req.mechanism
        )));
    }

    let mut out = DeadlinesResponse {
        generated_at: chrono::Utc::now(),
        state: "co".to_string(),
        injury_date: injury.to_string(),
        deadlines: Vec::new(),
        judgment_calls: Vec::new(),
        warnings: Vec::new(),
    };

    // ── the limitations period ────────────────────────────────────────────
    if is_mv {
        out.deadlines.push(Deadline {
            name: "Statute of limitations — motor vehicle tort".into(),
            citation: "C.R.S. 13-80-101(1)(n)(I)".into(),
            date: add_years(injury, 3).map(|d| d.to_string()),
            runs_from: "accrual (presumed here to be the injury date)".into(),
            period: "3 years".into(),
            fatal: true,
            note: Some(
                "Colorado gives motor-vehicle torts three years rather than the general two. The \
                 distinction turns on whether the injury arose from the use or operation of a \
                 motor vehicle, which is a legal determination — not every case involving a car \
                 qualifies."
                    .into(),
            ),
        });
        // Someone who assumes the general rule files a year early at worst and
        // abandons a live claim at best, so name the other period explicitly.
        out.deadlines.push(Deadline {
            name: "Statute of limitations — general tort (does NOT apply if the MV period governs)"
                .into(),
            citation: "C.R.S. 13-80-102(1)(a)".into(),
            date: add_years(injury, 2).map(|d| d.to_string()),
            runs_from: "accrual".into(),
            period: "2 years".into(),
            fatal: true,
            note: Some(
                "shown for contrast only. If the motor-vehicle characterisation is contested, the \
                 conservative course is to treat the earlier date as the operative one."
                    .into(),
            ),
        });
    } else {
        out.deadlines.push(Deadline {
            name: "Statute of limitations — general tort".into(),
            citation: "C.R.S. 13-80-102(1)(a)".into(),
            date: add_years(injury, 2).map(|d| d.to_string()),
            runs_from: "accrual (presumed here to be the injury date)".into(),
            period: "2 years".into(),
            fatal: true,
            note: Some(
                "if any motor vehicle was involved in the injury, the three-year period at \
                 13-80-101(1)(n)(I) may govern instead — re-run with mechanism=motor_vehicle."
                    .into(),
            ),
        });
    }

    // ── governmental immunity notice ──────────────────────────────────────
    if req.public_entity {
        out.deadlines.push(Deadline {
            name: "Governmental Immunity Act notice of claim".into(),
            citation: "C.R.S. 24-10-109".into(),
            date: injury.checked_add_signed(Duration::days(182)).map(|d| d.to_string()),
            runs_from: "discovery of the injury".into(),
            period: "182 days".into(),
            fatal: true,
            note: Some(
                "JURISDICTIONAL. Compliance is a condition of suit, and failure forever bars the \
                 claim regardless of merit — it cannot be cured. The period runs from discovery of \
                 the injury, which may be later than the injury date used here."
                    .into(),
            ),
        });
    } else {
        out.judgment_calls.push(
            "No public entity was indicated. Confirm that, because the 182-day C.R.S. 24-10-109 \
             notice is jurisdictional and reaches defendants people do not think of as \
             governmental — in Denver that includes RTD and Denver Health, as well as the City \
             and County itself and any public employee. If any of those may be implicated, re-run \
             with public_entity=true."
                .to_string(),
        );
    }

    // ── certificate of review ─────────────────────────────────────────────
    if req.professional_defendant {
        out.deadlines.push(Deadline {
            name: "Certificate of review".into(),
            citation: "C.R.S. 13-20-602".into(),
            date: service
                .and_then(|s| s.checked_add_signed(Duration::days(60)))
                .map(|d| d.to_string()),
            runs_from: "service of the complaint".into(),
            period: "60 days".into(),
            fatal: true,
            note: Some(if service.is_some() {
                "failure to file results in dismissal of the claim".into()
            } else {
                "date not computed: pass service_date. The period runs from SERVICE of the \
                 complaint, not from filing."
                    .to_string()
            }),
        });
    }

    // ── nonparty at fault ─────────────────────────────────────────────────
    out.deadlines.push(Deadline {
        name: "Designation of nonparties at fault".into(),
        citation: "C.R.S. 13-21-111.5(3)(b)".into(),
        date: filing
            .and_then(|f| f.checked_add_signed(Duration::days(90)))
            .map(|d| d.to_string()),
        runs_from: "commencement of the action".into(),
        period: "90 days".into(),
        fatal: false,
        note: Some(
            "primarily a defence deadline — it limits their ability to shift fault to an empty \
             chair — but worth calendaring so a late designation can be challenged. The court may \
             extend it."
                .into(),
        ),
    });

    // ── the rule-based trap we cannot source ──────────────────────────────
    out.deadlines.push(Deadline {
        name: "C.R.C.P. 16.1 simplified procedure — exclusion deadline".into(),
        citation: "C.R.C.P. 16.1".into(),
        date: None,
        runs_from: "the case being at issue".into(),
        period: "42 days (verify)".into(),
        fatal: false,
        note: Some(
            "NOT COMPUTED, deliberately. Simplified Procedure applies by DEFAULT and caps the \
             judgment at $100,000 unless the Civil Cover Sheet certifies over that at filing or a \
             party moves to exclude. Nothing errors when this is missed. The current Colorado \
             Rules of Civil Procedure are not available in a free machine-readable form — the only \
             free General Assembly PDF is the 2023 edition, three years stale — so this tool will \
             not quote a period it cannot verify as current. Read C.R.C.P. 16.1 directly."
                .into(),
        ),
    });

    // ── determinations the tool refuses to make ───────────────────────────
    out.judgment_calls.push(
        "Accrual is presumed here to equal the injury date. Under the discovery rule accrual can \
         be later — the claim accrues when the injury and its cause are known or should have been \
         known. Where that is in play, every date above shifts."
            .to_string(),
    );
    if req.professional_defendant && service.is_none() {
        out.warnings.push(
            "professional defendant indicated but no service_date given, so the certificate-of-\
             review date could not be computed"
                .to_string(),
        );
    }
    if filing.is_none() {
        out.warnings.push(
            "no filing_date given, so post-commencement deadlines were not computed".to_string(),
        );
    }
    out.warnings.push(
        "computed from statutory periods; tolling, minority, disability, and agreements between \
         the parties can all move these dates and are not modelled here"
            .to_string(),
    );

    Ok(out)
}

/// Add whole years, landing on the last valid day when the anniversary does
/// not exist. A Feb-29 injury has no Feb-29 anniversary in a non-leap year, and
/// returning None there would silently drop the limitations date — the one
/// output nobody can afford to lose.
fn add_years(d: NaiveDate, years: i32) -> Option<NaiveDate> {
    use chrono::Datelike as _;
    let year = d.year().checked_add(years)?;
    NaiveDate::from_ymd_opt(year, d.month(), d.day())
        .or_else(|| NaiveDate::from_ymd_opt(year, d.month(), d.day().saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(mechanism: &str, public_entity: bool) -> DeadlinesRequest {
        DeadlinesRequest {
            state: "co".into(),
            injury_date: "2024-03-15".into(),
            mechanism: mechanism.into(),
            public_entity,
            professional_defendant: false,
            filing_date: None,
            service_date: None,
        }
    }

    fn find<'a>(r: &'a DeadlinesResponse, needle: &str) -> &'a Deadline {
        r.deadlines
            .iter()
            .find(|d| d.name.contains(needle))
            .unwrap_or_else(|| panic!("no deadline matching {needle}: {:?}", r.deadlines))
    }

    /// The split is the single most-miscited Colorado PI fact.
    #[tokio::test]
    async fn motor_vehicle_gets_three_years_general_gets_two() {
        let mv = fetch(req("motor_vehicle", false)).await.expect("ok");
        assert_eq!(find(&mv, "motor vehicle tort").date.as_deref(), Some("2027-03-15"));

        let gen = fetch(req("premises", false)).await.expect("ok");
        assert_eq!(find(&gen, "general tort").date.as_deref(), Some("2026-03-15"));
    }

    /// On an MV case both periods are shown, so nobody assumes two years.
    #[tokio::test]
    async fn motor_vehicle_also_shows_the_general_period_for_contrast() {
        let mv = fetch(req("motor_vehicle", false)).await.expect("ok");
        let contrast = find(&mv, "does NOT apply");
        assert_eq!(contrast.date.as_deref(), Some("2026-03-15"));
    }

    #[tokio::test]
    async fn cgia_notice_is_182_days_and_flagged_fatal() {
        let r = fetch(req("general", true)).await.expect("ok");
        let n = find(&r, "Governmental Immunity");
        assert_eq!(n.date.as_deref(), Some("2024-09-13"));
        assert!(n.fatal);
        assert!(n.note.as_ref().is_some_and(|s| s.contains("JURISDICTIONAL")));
    }

    /// Not flagging a public entity must produce a prompt, not silence.
    #[tokio::test]
    async fn absent_public_entity_raises_the_question() {
        let r = fetch(req("general", false)).await.expect("ok");
        assert!(!r.deadlines.iter().any(|d| d.name.contains("Governmental")));
        assert!(
            r.judgment_calls.iter().any(|j| j.contains("RTD") && j.contains("Denver Health")),
            "must name the defendants people miss"
        );
    }

    #[tokio::test]
    async fn certificate_of_review_runs_from_service_not_filing() {
        let mut q = req("medical", false);
        q.professional_defendant = true;
        q.filing_date = Some("2025-01-10".into());
        q.service_date = Some("2025-02-01".into());
        let r = fetch(q).await.expect("ok");
        let c = find(&r, "Certificate of review");
        assert_eq!(c.date.as_deref(), Some("2025-04-02"));
    }

    #[tokio::test]
    async fn rule_16_1_is_declined_rather_than_guessed() {
        let r = fetch(req("general", false)).await.expect("ok");
        let d = find(&r, "16.1");
        assert!(d.date.is_none(), "must not compute from an unverifiable rule");
        assert!(d.note.as_ref().is_some_and(|s| s.contains("three years stale")));
    }

    #[tokio::test]
    async fn accrual_assumption_is_stated_not_hidden() {
        let r = fetch(req("general", false)).await.expect("ok");
        assert!(r.judgment_calls.iter().any(|j| j.contains("discovery rule")));
    }

    /// A leap-day injury has no anniversary in a non-leap year; dropping the
    /// date would silently remove the limitations deadline.
    #[tokio::test]
    async fn leap_day_injuries_still_produce_a_date() {
        let mut q = req("general", false);
        q.injury_date = "2024-02-29".into();
        let r = fetch(q).await.expect("ok");
        assert_eq!(find(&r, "general tort").date.as_deref(), Some("2026-02-28"));
    }

    #[tokio::test]
    async fn rejects_an_unknown_mechanism() {
        assert!(fetch(req("teleportation", false)).await.is_err());
    }
}
