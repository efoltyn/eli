//! Colorado damages caps, resolved by accrual date and filing date.
//!
//! This is the question the open web answers wrong most reliably, and the
//! reason is structural: the operative dollar figures are NOT in the statute.
//! C.R.S. 13-21-102.5 carries a base number and a set of date-keyed branches;
//! the adjusted amounts live in a Secretary of State certificate that is
//! reissued on a schedule. So a page that quotes the statute is stale, and a
//! page that quotes an old certificate is wrong, and neither looks wrong.
//!
//! Then HB 24-1472 made it worse in an interesting way: for claims accruing on
//! or after 2025-01-01 the indexing stops and a flat statutory figure applies —
//! and a civil action FILED during calendar 2025 gets that figure too, even on
//! an earlier accrual. So the same 2024 injury is capped at one number if suit
//! was filed in 2025 and a much lower one if filed in 2026. Two dates, not one.
//!
//! Figures below are transcribed from the SOS certificate revised 27 January
//! 2026 and from the statute text itself. They are a table rather than a live
//! PDF parse on purpose: mis-parsing a two-column PDF yields a plausible wrong
//! number, which is the single worst output this tool could produce. The
//! `certificate_revised` field and a freshness warning tell the caller when to
//! go re-read the source.

use super::{DamagesCap, DamagesCapRequest, DamagesCapResponse};
use crate::legal::parse_date;
use crate::{Error, Result};
use chrono::NaiveDate;

const CERT_URL: &str = "https://www.sos.state.co.us/pubs/info_center/files/damages_new.pdf";
/// The Governmental Immunity Act ceiling is certified separately, on its own
/// four-year cycle, in its own document. Same architecture as the noneconomic
/// cap — base figure in the statute, operative figure in a certificate.
const CGIA_CERT_URL: &str =
    "https://www.sos.state.co.us/pubs/info_center/files/LimitationsOnJudgments.pdf";
const CERT_REVISED: &str = "2026-01-27";
/// Indexing resumes on this date under HB 24-1472; past it, this table is
/// guaranteed stale and the tool says so instead of guessing.
const NEXT_ADJUSTMENT: &str = "2028-01-01";

/// One accrual band from the certificate. `until` is exclusive.
struct Band {
    from: &'static str,
    until: Option<&'static str>,
    noneconomic: i64,
    noneconomic_max: i64,
    wrongful_death: Option<i64>,
    solatium: Option<i64>,
    dram_shop: Option<i64>,
}

/// Transcribed from the SOS "Adjusted Limitations on Damages" certificate,
/// revised 2026-01-27. Bands are [from, until).
const BANDS: &[Band] = &[
    Band {
        from: "1998-01-01",
        until: Some("2008-01-01"),
        noneconomic: 366_250,
        noneconomic_max: 732_500,
        wrongful_death: Some(341_250),
        solatium: Some(68_250),
        dram_shop: Some(219_750),
    },
    Band {
        from: "2008-01-01",
        until: Some("2020-01-01"),
        noneconomic: 468_010,
        noneconomic_max: 936_030,
        wrongful_death: Some(436_070),
        solatium: Some(87_210),
        dram_shop: Some(280_810),
    },
    Band {
        from: "2020-01-01",
        until: Some("2022-01-01"),
        noneconomic: 613_760,
        noneconomic_max: 1_227_530,
        wrongful_death: Some(571_870),
        solatium: Some(114_370),
        dram_shop: Some(368_260),
    },
    Band {
        from: "2022-01-01",
        until: Some("2024-01-01"),
        noneconomic: 642_180,
        noneconomic_max: 1_284_370,
        wrongful_death: Some(598_350),
        solatium: Some(119_660),
        dram_shop: Some(385_310),
    },
    Band {
        from: "2024-01-01",
        until: Some("2025-01-01"),
        noneconomic: 729_790,
        noneconomic_max: 1_459_600,
        wrongful_death: Some(679_990),
        // Solatium was fixed at this point with no further adjustments.
        solatium: Some(135_990),
        dram_shop: Some(437_880),
    },
];

/// C.R.S. 24-10-114 ceilings, from the SOS "Limitations on Judgments"
/// certificate. These are a HARD CEILING on everything recoverable from a
/// public entity — not a noneconomic sub-limit — so against RTD, the City, or
/// Denver Health this number governs and 13-21-102.5 is irrelevant.
/// Bands are [from, until) by accrual date.
const CGIA_BANDS: &[(&str, Option<&str>, i64, i64)] = &[
    ("2018-01-01", Some("2022-01-01"), 387_000, 1_093_000),
    ("2022-01-01", Some("2026-01-01"), 424_000, 1_195_000),
    ("2026-01-01", Some("2030-01-01"), 505_000, 1_421_000),
];

/// Post-2025 figures come from the statute, not the certificate.
const STATUTORY_NONECONOMIC_2025: i64 = 1_500_000;
/// Solatium is fixed from 2024 on — the certificate says so in terms.
const SOLATIUM_FIXED_2024: i64 = 135_990;

pub(super) async fn fetch(req: DamagesCapRequest) -> Result<DamagesCapResponse> {
    let accrual = parse_date(&req.accrual_date, "accrual_date")?;
    let filing = match req.filing_date.as_deref() {
        Some(d) => Some(parse_date(d, "filing_date")?),
        None => None,
    };

    let mut out = DamagesCapResponse {
        generated_at: chrono::Utc::now(),
        state: "co".to_string(),
        accrual_date: accrual.to_string(),
        filing_date: filing.map(|d| d.to_string()),
        caps: Vec::new(),
        certificate_revised: Some(CERT_REVISED.to_string()),
        warnings: Vec::new(),
    };

    if filing.is_some_and(|f| f < accrual) {
        return Err(Error::InvalidInput(format!(
            "filing_date {} precedes accrual_date {} — a claim cannot be filed before it accrues",
            filing.expect("checked"),
            accrual
        )));
    }

    let claim = req.claim_type.trim().to_ascii_lowercase();
    let wanted: Vec<&str> = match claim.as_str() {
        "" | "all" => vec!["noneconomic", "wrongful_death", "solatium"],
        "noneconomic" | "pain_and_suffering" | "ne" => vec!["noneconomic"],
        "wrongful_death" | "wd" => vec!["wrongful_death"],
        "solatium" => vec!["solatium"],
        "dram_shop" | "dramshop" => vec!["dram_shop"],
        "cgia" | "public_entity" | "government" => vec!["cgia"],
        "medmal" | "medical_malpractice" => {
            // Medical malpractice is carved out of 13-21-102.5 entirely and
            // capped under 13-64-302 on a different schedule keyed to the acts
            // or omissions date. Answering it from this table would be wrong.
            out.warnings.push(
                "medical malpractice is expressly excluded from C.R.S. 13-21-102.5 and capped \
                 separately under C.R.S. 13-64-302, on a schedule keyed to the date of the acts \
                 or omissions rather than accrual. This tool does not compute it — read \
                 13-64-302 directly."
                    .to_string(),
            );
            return Ok(out);
        }
        other => {
            return Err(Error::InvalidInput(format!(
                "unknown claim_type {other:?}; use noneconomic, wrongful_death, solatium, \
                 dram_shop, medmal, or all"
            )))
        }
    };

    // The 2025 rule reaches back: a claim accruing on/after 2025-01-01, OR any
    // civil action filed during calendar 2025, takes the statutory figure.
    let bridge_start = date("2025-01-01");
    let bridge_end = date("2026-01-01");
    let filed_in_bridge = filing.is_some_and(|f| f >= bridge_start && f < bridge_end);
    let accrued_post_2025 = accrual >= bridge_start;
    let statutory_applies = accrued_post_2025 || filed_in_bridge;

    for want in wanted {
        out.caps.push(resolve(want, accrual, statutory_applies, filed_in_bridge, &mut out.warnings));
    }

    if filing.is_none() && accrual < bridge_start && accrual >= date("2024-01-01") {
        out.warnings.push(
            "no filing_date given, and this claim accrued in 2024 — the answer depends on it. \
             Filed during calendar 2025 the statutory figure applies; filed in 2026 or later the \
             2024 band does. Pass filing_date."
                .to_string(),
        );
    }
    if accrual < date("1998-01-01") {
        out.warnings.push(
            "the certificate's earliest band starts 1998-01-01; for an older accrual read the \
             statute as it stood at the time."
                .to_string(),
        );
    }
    if chrono::Utc::now().date_naive() >= date(NEXT_ADJUSTMENT) {
        out.warnings.push(format!(
            "inflation indexing resumed {NEXT_ADJUSTMENT} and this table was transcribed from the \
             certificate revised {CERT_REVISED} — re-read {CERT_URL} before relying on these figures."
        ));
    }
    if out.caps.iter().any(|c| c.claim_type != "cgia") {
        out.warnings.push(format!(
            "IF ANY DEFENDANT IS A PUBLIC ENTITY — RTD, a city or county, Denver Health, a school \
             district, the state — the C.R.S. 24-10-114 ceiling governs instead of anything above, \
             and it is far lower: a hard cap on TOTAL recovery from that defendant, not a \
             noneconomic sub-limit. Re-run with claim_type=cgia. Certified separately at \
             {CGIA_CERT_URL}."
        ));
    }
    out.warnings.push(format!(
        "figures transcribed from the Colorado SOS adjusted-limitations certificate revised \
         {CERT_REVISED} ({CERT_URL}) and from the statute; confirm against the certificate for \
         anything filed on."
    ));
    Ok(out)
}

fn resolve(
    kind: &str,
    accrual: NaiveDate,
    statutory_applies: bool,
    filed_in_bridge: bool,
    warnings: &mut Vec<String>,
) -> DamagesCap {
    let band = BANDS.iter().find(|b| {
        accrual >= date(b.from) && b.until.map(|u| accrual < date(u)).unwrap_or(true)
    });

    match kind {
        "noneconomic" => {
            if statutory_applies {
                let basis = if filed_in_bridge && accrual < date("2025-01-01") {
                    "civil action filed during calendar 2025 — C.R.S. 13-21-102.5(3)(c)(II) \
                     reaches back to earlier accruals filed in that window"
                        .to_string()
                } else {
                    "claim accrued on or after 2025-01-01 — flat statutory figure, indexing \
                     suspended until 2028"
                        .to_string()
                };
                return DamagesCap {
                    claim_type: "noneconomic".into(),
                    citation: "C.R.S. 13-21-102.5(3)(c)(II)".into(),
                    amount: Some(STATUTORY_NONECONOMIC_2025),
                    increased_maximum: None,
                    basis,
                    source: "statute".into(),
                    source_url: Some(
                        "https://olls.info/crs/crs2026-title-13.htm".into(),
                    ),
                };
            }
            match band {
                Some(b) => DamagesCap {
                    claim_type: "noneconomic".into(),
                    citation: "C.R.S. 13-21-102.5(3)(a)".into(),
                    amount: Some(b.noneconomic),
                    increased_maximum: Some(b.noneconomic_max),
                    basis: format!(
                        "accrual band {} to {}",
                        b.from,
                        b.until.unwrap_or("open")
                    ),
                    source: format!("SOS certificate revised {CERT_REVISED}"),
                    source_url: Some(CERT_URL.into()),
                },
                None => unresolved("noneconomic", "C.R.S. 13-21-102.5(3)(a)", warnings),
            }
        }
        "wrongful_death" => {
            if statutory_applies {
                warnings.push(
                    "wrongful-death claims accruing on or after 2025-01-01 (or filed during \
                     calendar 2025) take the amended C.R.S. 13-21-203 figure rather than a \
                     certificate band. This tool does not carry that number — read 13-21-203 \
                     directly. Note also that a felonious killing is uncapped."
                        .to_string(),
                );
                return unresolved("wrongful_death", "C.R.S. 13-21-203(1)", warnings);
            }
            match band.and_then(|b| b.wrongful_death.map(|v| (b, v))) {
                Some((b, v)) => DamagesCap {
                    claim_type: "wrongful_death".into(),
                    citation: "C.R.S. 13-21-203(1)".into(),
                    amount: Some(v),
                    increased_maximum: None,
                    basis: format!("accrual band {} to {}", b.from, b.until.unwrap_or("open")),
                    source: format!("SOS certificate revised {CERT_REVISED}"),
                    source_url: Some(CERT_URL.into()),
                },
                None => unresolved("wrongful_death", "C.R.S. 13-21-203(1)", warnings),
            }
        }
        "solatium" => {
            // Fixed from 2024 forward; the certificate says there will be no
            // additional adjustments, so this one does not move.
            if accrual >= date("2024-01-01") {
                return DamagesCap {
                    claim_type: "solatium".into(),
                    citation: "C.R.S. 13-21-203.5".into(),
                    amount: Some(SOLATIUM_FIXED_2024),
                    increased_maximum: None,
                    basis: "fixed for accruals on or after 2024-01-01; the certificate states \
                            there will be no additional adjustments"
                        .into(),
                    source: format!("SOS certificate revised {CERT_REVISED}"),
                    source_url: Some(CERT_URL.into()),
                };
            }
            match band.and_then(|b| b.solatium.map(|v| (b, v))) {
                Some((b, v)) => DamagesCap {
                    claim_type: "solatium".into(),
                    citation: "C.R.S. 13-21-203.5".into(),
                    amount: Some(v),
                    increased_maximum: None,
                    basis: format!("accrual band {} to {}", b.from, b.until.unwrap_or("open")),
                    source: format!("SOS certificate revised {CERT_REVISED}"),
                    source_url: Some(CERT_URL.into()),
                },
                None => unresolved("solatium", "C.R.S. 13-21-203.5", warnings),
            }
        }
        "cgia" => {
            match CGIA_BANDS.iter().find(|(from, until, _, _)| {
                accrual >= date(from) && until.map(|u| accrual < date(u)).unwrap_or(true)
            }) {
                Some((from, until, per_person, per_occurrence)) => DamagesCap {
                    claim_type: "cgia".into(),
                    citation: "C.R.S. 24-10-114(1) as adjusted".into(),
                    amount: Some(*per_person),
                    // Not a court-raised maximum — this is the multi-claimant
                    // ceiling for the whole occurrence, and no single person may
                    // exceed the per-person figure out of it.
                    increased_maximum: Some(*per_occurrence),
                    basis: format!(
                        "accrual band {from} to {}; per-person ceiling, with the second figure the \
                         cap for an occurrence injuring two or more people",
                        until.unwrap_or("open")
                    ),
                    source: "SOS Limitations on Judgments certificate".into(),
                    source_url: Some(CGIA_CERT_URL.into()),
                },
                None => unresolved("cgia", "C.R.S. 24-10-114(1)", warnings),
            }
        }
        "dram_shop" => match band.and_then(|b| b.dram_shop.map(|v| (b, v))) {
            Some((b, v)) => DamagesCap {
                claim_type: "dram_shop".into(),
                citation: "C.R.S. 44-3-801(3)(c) & (4)(c)".into(),
                amount: Some(v),
                increased_maximum: None,
                basis: format!("accrual band {} to {}", b.from, b.until.unwrap_or("open")),
                source: format!("SOS certificate revised {CERT_REVISED}"),
                source_url: Some(CERT_URL.into()),
            },
            None => unresolved("dram_shop", "C.R.S. 44-3-801", warnings),
        },
        other => unresolved(other, "", warnings),
    }
}

/// Return a cap with no amount rather than a guess. A null here is a caller's
/// cue to go read the source; a plausible wrong number is not.
fn unresolved(kind: &str, citation: &str, warnings: &mut Vec<String>) -> DamagesCap {
    warnings.push(format!(
        "no {kind} figure resolved for this accrual date — read {citation} and the certificate at \
         {CERT_URL} rather than treating this as uncapped"
    ));
    DamagesCap {
        claim_type: kind.to_string(),
        citation: citation.to_string(),
        amount: None,
        increased_maximum: None,
        basis: "not resolved from the certificate bands or the statute".into(),
        source: "unresolved".into(),
        source_url: Some(CERT_URL.into()),
    }
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("const date literal")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(accrual: &str, filing: Option<&str>, kind: &str) -> DamagesCapRequest {
        DamagesCapRequest {
            state: "co".into(),
            claim_type: kind.into(),
            accrual_date: accrual.into(),
            filing_date: filing.map(str::to_string),
        }
    }

    async fn one(accrual: &str, filing: Option<&str>, kind: &str) -> DamagesCap {
        let r = fetch(req(accrual, filing, kind)).await.expect("resolves");
        r.caps.into_iter().next().expect("a cap")
    }

    #[tokio::test]
    async fn picks_the_band_by_accrual_date() {
        assert_eq!(one("2021-06-01", None, "noneconomic").await.amount, Some(613_760));
        assert_eq!(one("2023-03-15", None, "noneconomic").await.amount, Some(642_180));
        assert_eq!(one("2019-12-31", None, "noneconomic").await.amount, Some(468_010));
    }

    /// Band edges are [from, until) — an accrual exactly on a boundary belongs
    /// to the NEW band. Off-by-one here is a six-figure error.
    #[tokio::test]
    async fn band_boundaries_are_half_open() {
        assert_eq!(one("2020-01-01", None, "noneconomic").await.amount, Some(613_760));
        assert_eq!(one("2019-12-31", None, "noneconomic").await.amount, Some(468_010));
        assert_eq!(one("2022-01-01", None, "noneconomic").await.amount, Some(642_180));
    }

    #[tokio::test]
    async fn post_2025_accrual_takes_the_flat_statutory_figure() {
        let c = one("2025-04-01", None, "noneconomic").await;
        assert_eq!(c.amount, Some(1_500_000));
        assert!(c.citation.contains("13-21-102.5(3)(c)(II)"));
    }

    /// The bridge is the whole point: same injury, filing date decides.
    #[tokio::test]
    async fn the_2025_filing_bridge_reaches_back_to_a_2024_accrual() {
        let filed_2025 = one("2024-06-01", Some("2025-07-01"), "noneconomic").await;
        let filed_2026 = one("2024-06-01", Some("2026-02-01"), "noneconomic").await;
        assert_eq!(filed_2025.amount, Some(1_500_000));
        assert_eq!(filed_2026.amount, Some(729_790));
        assert!(
            filed_2025.amount > filed_2026.amount,
            "the bridge must be worth more, or the rule is inverted"
        );
    }

    #[tokio::test]
    async fn warns_when_a_2024_accrual_has_no_filing_date() {
        let r = fetch(req("2024-06-01", None, "noneconomic")).await.expect("ok");
        assert!(
            r.warnings.iter().any(|w| w.contains("depends on it")),
            "must flag the missing filing_date: {:?}",
            r.warnings
        );
    }

    #[tokio::test]
    async fn solatium_is_fixed_from_2024() {
        assert_eq!(one("2024-02-01", None, "solatium").await.amount, Some(135_990));
        assert_eq!(one("2030-02-01", None, "solatium").await.amount, Some(135_990));
        assert_eq!(one("2021-02-01", None, "solatium").await.amount, Some(114_370));
    }

    #[tokio::test]
    async fn medmal_refuses_rather_than_answering_from_the_wrong_statute() {
        let r = fetch(req("2023-01-01", None, "medmal")).await.expect("ok");
        assert!(r.caps.is_empty());
        assert!(r.warnings.iter().any(|w| w.contains("13-64-302")));
    }

    #[tokio::test]
    async fn rejects_a_filing_date_before_accrual() {
        assert!(fetch(req("2024-06-01", Some("2024-01-01"), "noneconomic")).await.is_err());
    }

    #[tokio::test]
    async fn post_2025_wrongful_death_declines_rather_than_guessing() {
        let c = one("2025-06-01", None, "wrongful_death").await;
        assert_eq!(c.amount, None, "must not invent a post-amendment figure");
    }

    /// The defect this fixes: a 2026 crash with RTD as defendant returned
    /// $1.5M, three times the real $505,000 ceiling, with nothing pointing at
    /// the governing statute.
    #[tokio::test]
    async fn cgia_ceiling_applies_to_public_entity_defendants() {
        let c = one("2026-04-10", None, "cgia").await;
        assert_eq!(c.amount, Some(505_000));
        assert_eq!(c.increased_maximum, Some(1_421_000));
        assert!(c.citation.contains("24-10-114"));
    }

    #[tokio::test]
    async fn cgia_bands_step_by_accrual_date() {
        assert_eq!(one("2025-12-31", None, "cgia").await.amount, Some(424_000));
        assert_eq!(one("2026-01-01", None, "cgia").await.amount, Some(505_000));
        assert_eq!(one("2021-06-01", None, "cgia").await.amount, Some(387_000));
    }

    /// Not knowing to ask is the actual failure mode, so every non-CGIA answer
    /// has to raise the possibility itself.
    #[tokio::test]
    async fn a_noneconomic_answer_warns_about_the_public_entity_ceiling() {
        let r = fetch(req("2026-04-10", None, "noneconomic")).await.expect("ok");
        assert_eq!(r.caps[0].amount, Some(1_500_000));
        assert!(
            r.warnings.iter().any(|w| w.contains("24-10-114") && w.contains("RTD")),
            "must flag the CGIA ceiling: {:?}",
            r.warnings
        );
    }

    #[tokio::test]
    async fn all_returns_the_three_common_caps() {
        let r = fetch(req("2023-01-01", None, "all")).await.expect("ok");
        assert_eq!(r.caps.len(), 3);
    }
}
