//! Massachusetts General Laws — the codified text of a chapter-and-section cite.
//!
//! Why this exists when the General Laws are already on the open web: the
//! statute a Massachusetts driver is actually charged under (ch. 90, § 17) is
//! served by malegislature.gov as JSON, key-free, one section per call — and
//! with an explicit `IsRepealed` flag. That flag is the reason this source was
//! picked over scraping the HTML: a repealed section still has a page, still
//! reads like law, and comes back from a search engine looking current. Here it
//! arrives as a boolean, and repealed sections carry an *empty* `Text` with the
//! repeal note in `Name` — so the failure a scraper would return as "the law
//! says nothing" is something this module can name.
//!
//! The other half of the job is the citation itself. Massachusetts addresses law
//! as chapter + section, and a model will hand that over in every shape it has
//! ever seen: two separate fields, "90/17", "90:17", "ch. 90 s. 17". All of them
//! resolve here, and any reading that required a guess is stated in `warnings` —
//! necessary because MGL genuinely contains section codes with slashes in them
//! (§ 7D1/2, § 31/2) and even chapters with slashes (ch. 92A1/2), so "31/2" is
//! honestly ambiguous between one fractional section and chapter 31, § 2.
//!
//! No API key. Base: https://malegislature.gov/api

use super::{StateStatuteRequest, StateStatuteResponse};
use crate::legal::{clamp_text, shared_client, soft_fail, strip_tags_keep_lines};
use crate::{Error, Result};
use chrono::Utc;
use regex::Regex;
use serde::Deserialize;
use std::sync::LazyLock;

const MA_API: &str = "https://malegislature.gov/api";
const SOURCE: &str = "malegislature.gov API";

/// `max_chars: 0` means the caller never set one. Clamping to zero would return
/// an empty `text` for a live section — indistinguishable from the repealed
/// case this module works hard to flag — so treat it as unset.
const DEFAULT_MAX_CHARS: usize = 50_000;

/// Hard ceiling on any body read from this host. Sections run 0.2–70 KB and a
/// chapter listing 2–200 KB, but the same API also serves `/api/Documents`,
/// which streams 12.3 MB and ignores `?limit=`; a mistyped path should cost a
/// few kilobytes, not a full download.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The subset of the section payload this module uses. The response also
/// carries `Chapter` and `Part` link objects; ignoring unknown fields keeps a
/// schema addition upstream from turning into a parse failure here.
#[derive(Debug, Deserialize)]
struct MaSection {
    #[serde(rename = "Code")]
    code: Option<String>,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "IsRepealed")]
    is_repealed: Option<bool>,
    #[serde(rename = "Text")]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MaChapter {
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Sections")]
    sections: Option<Vec<MaSectionStub>>,
}

#[derive(Debug, Deserialize)]
struct MaSectionStub {
    #[serde(rename = "Code")]
    code: Option<String>,
}

pub(super) async fn fetch_statute(req: StateStatuteRequest) -> Result<StateStatuteResponse> {
    let cite = parse_citation(&req.section, req.chapter.as_deref())?;

    let mut out = StateStatuteResponse {
        generated_at: Utc::now(),
        state: super::normalize_state(&req.state),
        citation: Some(format_citation(&cite.chapter, &cite.section)),
        heading: None,
        text: None,
        chars: 0,
        truncated: false,
        repealed: None,
        source: Some(SOURCE.to_string()),
        source_url: None,
        warnings: cite.notes.clone(),
    };

    let url = section_url(&cite.chapter, &cite.section);
    out.source_url = Some(url.clone());

    // Sections are small; the general client's timeout is the right one.
    let Some(body) =
        get_capped(&shared_client::GENERAL, &url, "malegislature section", &mut out.warnings).await
    else {
        explain_miss(&cite, &mut out.warnings).await;
        return Ok(out);
    };

    let max_chars = if req.max_chars == 0 { DEFAULT_MAX_CHARS } else { req.max_chars };
    apply_section(&cite, &body, max_chars, &mut out);
    Ok(out)
}

/// Fill the response from a section payload. Split out from the fetch so the
/// repealed / empty-text / malformed-body paths are testable without network.
fn apply_section(cite: &Citation, body: &str, max_chars: usize, out: &mut StateStatuteResponse) {
    let parsed: MaSection = match serde_json::from_str(body) {
        Ok(p) => p,
        Err(e) => {
            // A 200 that isn't the expected JSON leaves us with nothing to
            // quote. Say so instead of returning an absent `text` silently.
            out.warnings.push(format!(
                "malegislature section: 200 response was not the expected JSON ({e}); no statutory text returned"
            ));
            return;
        }
    };

    // The API echoes the canonical code — "7D1/2" for the "7D1~2" we had to ask
    // for — so cite what the source calls the section, not what we typed.
    let code = parsed
        .code
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or(cite.section.as_str());
    let citation = format_citation(&cite.chapter, code);
    out.citation = Some(citation.clone());
    out.heading = parsed
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string);

    let repealed = parsed.is_repealed.unwrap_or(false);
    out.repealed = Some(repealed);
    if repealed {
        // Worth a warning on top of the field: a summarizer that reads only the
        // text would otherwise present a repealed section as current law.
        out.warnings.push(format!(
            "{citation} is marked REPEALED by the source{} — do not quote it as current law",
            out.heading
                .as_deref()
                .map(|h| format!(" ({h})"))
                .unwrap_or_default()
        ));
    }

    let text = parsed.text.as_deref().map(clean_body).unwrap_or_default();
    if text.is_empty() {
        // Repealed sections come back with `Text: ""`. Leaving `text` as an
        // empty string would read as "this section says nothing", which is a
        // statement about the law rather than about the source.
        out.warnings.push(format!(
            "{citation}: the source returned no text for this section{}",
            if repealed {
                " — repealed entries carry only the repeal note in the heading"
            } else {
                ""
            }
        ));
        return;
    }

    let (clamped, truncated) = clamp_text(&text, max_chars);
    out.chars = text.chars().count();
    out.truncated = truncated;
    out.text = Some(clamped);
}

/// Turn a section miss into something correctable.
///
/// The section endpoint answers "no such section" and "no such chapter" with
/// the identical 400 body ("No General Law Sections were found."), so on its own
/// the caller cannot tell which half of the citation was wrong. One chapter
/// listing separates them and names the codes that do exist. Only spent on the
/// failure path — the happy path stays at exactly one request.
async fn explain_miss(cite: &Citation, warnings: &mut Vec<String>) {
    let url = chapter_url(&cite.chapter);
    let Some(body) =
        get_capped(&shared_client::BULK, &url, "malegislature chapter", warnings).await
    else {
        warnings.push(format!(
            "ch. {} did not resolve either — MGL chapters run 1 to 282 plus lettered ones (6A, 21E) \
             and a few fractional ones (92A1/2)",
            cite.chapter
        ));
        return;
    };

    let Ok(chapter) = serde_json::from_str::<MaChapter>(&body) else {
        return;
    };
    let codes: Vec<String> = chapter
        .sections
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.code)
        .collect();
    let name = chapter
        .name
        .as_deref()
        .map(|n| format!(" ({n})"))
        .unwrap_or_default();
    if codes.is_empty() {
        warnings.push(format!("ch. {}{name} exists but lists no sections", cite.chapter));
        return;
    }
    warnings.push(format!(
        "ch. {}{name} exists but has no § {}; {} sections in the chapter, nearest codes: {}",
        cite.chapter,
        cite.section,
        codes.len(),
        nearby_codes(&codes, &cite.section).join(", ")
    ));
}

// ── citation parsing ───────────────────────────────────────────────────────

/// A citation resolved to the two things the API needs, plus what had to be
/// assumed to get there.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Citation {
    chapter: String,
    section: String,
    notes: Vec<String>,
}

/// How a chapter was recovered from the section string. The distinction is
/// load-bearing: a `/` inside a section string may be a chapter separator
/// ("90/17") or part of a real section code ("31/2" = § 31 1/2), and only an
/// explicit chapter or a written label settles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// "ch. 90 s. 17", "Mass. Gen. Laws ch. 90, § 17" — written out.
    Labeled,
    /// "90:17", "90-17" — split on a character no MGL code contains.
    Punct,
    /// "90/17" — split on a character MGL codes *do* contain.
    Slash,
    /// "90 17" — two bare tokens.
    Space,
    /// No chapter anywhere in the string.
    Bare,
}

fn parse_citation(section: &str, chapter: Option<&str>) -> Result<Citation> {
    let raw = section.trim();
    if raw.is_empty() {
        return Err(Error::InvalidInput(
            "Massachusetts statute lookup needs a section, e.g. chapter \"90\" section \"17\" \
             for Mass. Gen. Laws ch. 90, § 17"
                .into(),
        ));
    }
    let (scanned_chapter, scanned_section, origin) = scan(raw);
    let explicit = chapter
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(normalize_chapter)
        .transpose()?;

    let mut notes = Vec::new();
    let Some(chapter) = explicit else {
        let chapter = scanned_chapter.ok_or_else(|| missing_chapter(raw))?;
        match origin {
            // Only the shapes that required a guess get a note; ':' and a
            // written "ch." are unambiguous.
            Origin::Slash => notes.push(format!(
                "read section {raw:?} as ch. {chapter}, § {scanned_section}; MGL also has real \
                 section codes containing '/' (§ 7D1/2, § 31/2), so pass the chapter separately \
                 to remove the ambiguity"
            )),
            Origin::Space => notes.push(format!(
                "read section {raw:?} as ch. {chapter}, § {scanned_section}"
            )),
            _ => {}
        }
        return Ok(Citation { chapter, section: scanned_section, notes });
    };

    // With an explicit chapter in hand, a separator inside the section string is
    // only a separator when it agrees with that chapter — otherwise "31/2" under
    // chapter 90 would be shredded into § 2 of a chapter nobody asked for.
    let section = match origin {
        Origin::Labeled | Origin::Punct => {
            if let Some(found) = scanned_chapter.as_deref() {
                if found != chapter {
                    notes.push(format!(
                        "chapter {chapter:?} and section {raw:?} name different chapters; used \
                         ch. {chapter}, § {scanned_section} — pass one or the other, not both"
                    ));
                }
            }
            scanned_section
        }
        Origin::Slash | Origin::Space => match scanned_chapter.as_deref() {
            // "chapter 90 + section 90/17" — the model repeated itself.
            Some(found) if found == chapter => {
                notes.push(format!(
                    "section {raw:?} repeated the chapter; read as ch. {chapter}, § {scanned_section}"
                ));
                scanned_section
            }
            // "chapter 90 + section 31/2" — a fractional code, kept whole.
            _ => clean_section(raw),
        },
        Origin::Bare => scanned_section,
    };

    if section.is_empty() {
        return Err(missing_chapter(raw));
    }
    Ok(Citation { chapter, section, notes })
}

fn missing_chapter(raw: &str) -> Error {
    Error::InvalidInput(format!(
        "Massachusetts law is cited as chapter + section, and {raw:?} names only a section. \
         Pass chapter \"90\" with section \"17\", or write both into the section as \"90/17\", \
         \"90:17\" or \"ch. 90 s. 17\" — all four mean Mass. Gen. Laws ch. 90, § 17 (speeding)."
    ))
}

/// Written-out chapter: "ch. 90", "chapter 21E", "M.G.L. c.90".
static CHAPTER_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bc(?:h(?:apter)?)?\.?\s*(\d+[a-z]*(?:\d*/\d+)?)\b").expect("static regex")
});

/// Written-out section: "§ 17", "sec. 17", "s. 17", "section 50A to 50L".
static SECTION_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:§+|\bsec(?:tion|t)?\.?|\bs\.)\s*(\d+[a-z0-9/]*(?:\s+to\s+\d+[a-z]*)?)")
        .expect("static regex")
});

/// Pull a chapter and a section out of one string, reporting which shape it was
/// written in so the caller can decide how much to trust the split.
fn scan(raw: &str) -> (Option<String>, String, Origin) {
    let s = raw.trim();

    if let Some(sec) = SECTION_LABEL.captures(s) {
        let section = clean_section(&sec[1]);
        let chapter = CHAPTER_LABEL
            .captures(s)
            .map(|c| c[1].to_string())
            .or_else(|| {
                // "90 § 17": the chapter is the unlabelled number in front of
                // the section label.
                let head = sec.get(0).map(|m| s[..m.start()].trim()).unwrap_or("");
                let head = head.trim_end_matches([',', ';', '.']).trim();
                (!head.is_empty()).then(|| head.to_string())
            })
            .and_then(|c| normalize_chapter(&c).ok());
        return (chapter, section, Origin::Labeled);
    }

    if let Some(ch) = CHAPTER_LABEL.captures(s) {
        let rest = ch
            .get(0)
            .map(|m| clean_section(&s[m.end()..]))
            .unwrap_or_default();
        return (normalize_chapter(&ch[1]).ok(), rest, Origin::Labeled);
    }

    // ':' and '-' never appear inside an MGL code, so a split on them is safe.
    if let Some(i) = s.find([':', '-']) {
        let (head, tail) = s.split_at(i);
        if let Ok(chapter) = normalize_chapter(head) {
            return (Some(chapter), clean_section(&tail[1..]), Origin::Punct);
        }
    }
    // '/' does appear inside codes (§ 7D1/2), so this split is a reading, not a
    // fact — Origin::Slash is what makes the caller say so.
    if let Some(i) = s.find('/') {
        let (head, tail) = s.split_at(i);
        if let Ok(chapter) = normalize_chapter(head) {
            let tail = clean_section(&tail[1..]);
            if !tail.is_empty() {
                return (Some(chapter), tail, Origin::Slash);
            }
        }
    }
    // "90 17", but not "50A to 50L", which is one real section code.
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.len() == 2 && !tokens.iter().any(|t| t.eq_ignore_ascii_case("to")) {
        if let Ok(chapter) = normalize_chapter(tokens[0]) {
            return (Some(chapter), clean_section(tokens[1]), Origin::Space);
        }
    }

    (None, clean_section(s), Origin::Bare)
}

/// Chapter codes are digits, optionally lettered (21E) and very occasionally
/// fractional (92A1/2). Anything else is a caller error, not a lookup to try.
fn normalize_chapter(raw: &str) -> Result<String> {
    let cleaned: String = raw
        .trim()
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/')
        .to_ascii_uppercase();
    let stripped = strip_word(&cleaned, &["CHAPTER", "CHAP", "CH", "C"]);
    let ok = !stripped.is_empty()
        && stripped.chars().next().is_some_and(|c| c.is_ascii_digit())
        && stripped
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '/');
    if !ok {
        return Err(Error::InvalidInput(format!(
            "{raw:?} is not a Massachusetts chapter; chapters look like \"90\", \"21E\" or \"92A1/2\""
        )));
    }
    Ok(stripped)
}

/// Strip a leading word (with any trailing '.' or whitespace) from a code.
fn strip_word(input: &str, words: &[&str]) -> String {
    for w in words {
        if let Some(rest) = input.strip_prefix(w) {
            let rest = rest.trim_start_matches(['.', ' ', '\t']);
            if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return rest.to_string();
            }
        }
    }
    input.to_string()
}

/// Normalize a section code to the form the API indexes: uppercase, single
/// spaces, no leading "§"/"section" label and no trailing punctuation. The
/// "to" in a range code ("50A to 50L") stays lowercase — that is how the source
/// spells it.
fn clean_section(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/');
    let upper = trimmed.to_ascii_uppercase();
    let stripped = strip_word(&upper, &["SECTIONS", "SECTION", "SECT", "SEC", "S"]);
    stripped
        .split_whitespace()
        .map(|t| if t == "TO" { "to".to_string() } else { t.to_string() })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The form a Massachusetts brief would use.
fn format_citation(chapter: &str, section: &str) -> String {
    format!("Mass. Gen. Laws ch. {chapter}, § {section}")
}

// ── http ───────────────────────────────────────────────────────────────────

/// This API spells a `/` inside a code as `~`: § 7D1/2 lives at
/// `.../Sections/7D1~2`. Percent-encoding the slash instead is rejected by IIS
/// with a 404 before the API ever sees it, and MGL has both fractional sections
/// and fractional chapters (ch. 92A1/2), so both halves of the path need it.
/// Spaces (the "36 to 38" range codes) do percent-encode normally.
fn encode_code(code: &str) -> String {
    urlencoding::encode(&code.replace('/', "~")).into_owned()
}

fn section_url(chapter: &str, section: &str) -> String {
    format!(
        "{MA_API}/Chapters/{}/Sections/{}",
        encode_code(chapter),
        encode_code(section)
    )
}

fn chapter_url(chapter: &str) -> String {
    format!("{MA_API}/Chapters/{}", encode_code(chapter))
}

/// GET a body with a hard size ceiling.
///
/// Read in chunks rather than `.text()` because this host serves at least one
/// endpoint (`/api/Documents`) that streams 12.3 MB and ignores every limit
/// parameter; a wrong path should be cheap.
async fn get_capped(
    client: &reqwest::Client,
    url: &str,
    source: &str,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("{source} request failed: {e}"));
            return None;
        }
    };
    let mut resp = soft_fail(source, resp, warnings).await?;

    let mut buf: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                buf.extend_from_slice(&chunk);
                if buf.len() > MAX_BODY_BYTES {
                    warnings.push(format!(
                        "{source}: response exceeded {MAX_BODY_BYTES} bytes and was cut off"
                    ));
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                warnings.push(format!("{source} body read failed: {e}"));
                return None;
            }
        }
    }
    match String::from_utf8(buf) {
        Ok(s) => Some(s),
        Err(e) => {
            warnings.push(format!("{source}: body was not valid UTF-8 ({e})"));
            None
        }
    }
}

/// Statute text arrives as plain text with CRLF paragraph breaks and a few HTML
/// entities (`&mdash;` opens most definitional sections). Keep the line
/// structure — statutory subsections welded into one paragraph are unreadable —
/// and decode entities *after* stripping, so a literal `&lt;` in the text of a
/// law is not turned into a tag and then deleted.
fn clean_body(raw: &str) -> String {
    let normalized = raw.replace("\r\n", "\n");
    let stripped = strip_tags_keep_lines(&normalized);
    html_escape::decode_html_entities(&stripped).trim().to_string()
}

/// Section codes closest to the one that missed, keeping the chapter's own
/// order so the list reads like the table of contents it came from.
fn nearby_codes(codes: &[String], want: &str) -> Vec<String> {
    let want = want.to_ascii_uppercase();
    // Longest shared prefix first, then the closest length — without that
    // second key, "§ 1" ties with "§ 16" against a miss on "17Z" and wins on
    // position, which is not what a typo meant.
    let mut ranked: Vec<(usize, usize, usize, &String)> = codes
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let upper = c.to_ascii_uppercase();
            let gap = upper.chars().count().abs_diff(want.chars().count());
            (common_prefix_len(&upper, &want), gap, i, c)
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut picked: Vec<(usize, &String)> =
        ranked.into_iter().take(6).map(|(_, _, i, c)| (i, c)).collect();
    picked.sort_by_key(|(i, _)| *i);
    picked.into_iter().map(|(_, c)| c.clone()).collect()
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cite(section: &str, chapter: Option<&str>) -> Citation {
        parse_citation(section, chapter).expect("citation parses")
    }

    #[test]
    fn parses_every_form_a_model_writes() {
        for form in ["90/17", "90:17", "90-17", "90 17", "ch. 90 s. 17"] {
            let c = cite(form, None);
            assert_eq!((c.chapter.as_str(), c.section.as_str()), ("90", "17"), "{form}");
        }
        let c = cite("Mass. Gen. Laws ch. 90, § 17", None);
        assert_eq!((c.chapter.as_str(), c.section.as_str()), ("90", "17"));
        let c = cite("M.G.L. c.90 §17", None);
        assert_eq!((c.chapter.as_str(), c.section.as_str()), ("90", "17"));
        // Separate fields, with or without decoration on either one.
        let c = cite("17", Some("90"));
        assert_eq!((c.chapter.as_str(), c.section.as_str()), ("90", "17"));
        assert!(c.notes.is_empty(), "the unambiguous form needs no warning");
        let c = cite("§ 17", Some("ch. 90"));
        assert_eq!((c.chapter.as_str(), c.section.as_str()), ("90", "17"));
        // Lettered chapters and lettered sections survive the round trip.
        let c = cite("21E/3", None);
        assert_eq!((c.chapter.as_str(), c.section.as_str()), ("21E", "3"));
        let c = cite("17a", Some("90"));
        assert_eq!(c.section, "17A");
    }

    #[test]
    fn ambiguous_forms_say_how_they_were_read() {
        // '/' is a real character in MGL section codes, so splitting on it is a
        // reading and has to be disclosed.
        let c = cite("90/17", None);
        assert_eq!(c.notes.len(), 1);
        assert!(c.notes[0].contains("ch. 90, § 17"), "{:?}", c.notes);
        // ':' cannot occur inside a code, so that split is a fact.
        assert!(cite("90:17", None).notes.is_empty());
        assert!(cite("ch. 90 s. 17", None).notes.is_empty());
    }

    #[test]
    fn an_explicit_chapter_protects_fractional_section_codes() {
        // Bare "31/2" reads as chapter 31, section 2 — the only thing it can
        // mean without a chapter, and it says so.
        let bare = cite("31/2", None);
        assert_eq!((bare.chapter.as_str(), bare.section.as_str()), ("31", "2"));
        assert!(!bare.notes.is_empty());
        // With the chapter supplied, § 31/2 of ch. 90 stays whole.
        let held = cite("31/2", Some("90"));
        assert_eq!((held.chapter.as_str(), held.section.as_str()), ("90", "31/2"));
        let held = cite("7D1/2", Some("90"));
        assert_eq!(held.section, "7D1/2");
        // A repeated chapter is recognized rather than treated as a code.
        let repeated = cite("90/17", Some("90"));
        assert_eq!((repeated.chapter.as_str(), repeated.section.as_str()), ("90", "17"));
        assert!(repeated.notes[0].contains("repeated the chapter"), "{:?}", repeated.notes);
    }

    #[test]
    fn range_codes_are_not_mistaken_for_two_tokens() {
        let c = cite("50A to 50L", Some("90"));
        assert_eq!(c.section, "50A to 50L");
        let c = cite("90/50A to 50L", None);
        assert_eq!((c.chapter.as_str(), c.section.as_str()), ("90", "50A to 50L"));
    }

    #[test]
    fn section_without_a_chapter_is_a_caller_error_with_a_worked_example() {
        let err = parse_citation("17", None).expect_err("MA needs a chapter");
        let msg = err.to_string();
        assert!(msg.contains("chapter"), "{msg}");
        assert!(msg.contains("90/17") && msg.contains("ch. 90 s. 17"), "{msg}");
        assert!(parse_citation("   ", None).is_err());
        assert!(parse_citation("17", Some("not-a-chapter")).is_err());
    }

    #[test]
    fn citation_formatting() {
        assert_eq!(format_citation("90", "17"), "Mass. Gen. Laws ch. 90, § 17");
        assert_eq!(format_citation("21E", "3"), "Mass. Gen. Laws ch. 21E, § 3");
    }

    #[test]
    fn urls_use_the_tilde_the_api_wants_for_slashes() {
        assert_eq!(
            section_url("90", "17"),
            "https://malegislature.gov/api/Chapters/90/Sections/17"
        );
        // A percent-encoded slash 404s at the web server; '~' is what resolves.
        assert_eq!(
            section_url("90", "7D1/2"),
            "https://malegislature.gov/api/Chapters/90/Sections/7D1~2"
        );
        assert_eq!(
            section_url("92A1/2", "1"),
            "https://malegislature.gov/api/Chapters/92A1~2/Sections/1"
        );
        assert_eq!(
            section_url("90", "50A to 50L"),
            "https://malegislature.gov/api/Chapters/90/Sections/50A%20to%2050L"
        );
        assert_eq!(chapter_url("90"), "https://malegislature.gov/api/Chapters/90");
    }

    /// Trimmed from the live payload of ch. 90, § 17.
    const LIVE_17: &str = r#"{"Code":"17","Name":"Speed limits","IsRepealed":false,
        "Text":"Section 17. No person operating a motor vehicle on any way shall run it at a rate of speed greater than is reasonable and proper.\r\n\r\nUnless a way is otherwise posted&mdash;it shall be prima facie evidence.",
        "Chapter":{"Code":"90"},"Part":{"Code":"I"}}"#;

    /// Trimmed from the live payload of ch. 90, §§ 50A to 50L — repealed, and
    /// note the empty Text that comes with it.
    const LIVE_REPEALED: &str = r#"{"Code":"50A to 50L","Name":"Inoperative February 17, 1959 upon title vesting in the Massachusetts Port Authority. See 1956, 465, Sec. 32","IsRepealed":true,"Text":"","Chapter":{"Code":"90"},"Part":{"Code":"I"}}"#;

    fn blank(cite: &Citation) -> StateStatuteResponse {
        StateStatuteResponse {
            generated_at: Utc::now(),
            state: "ma".into(),
            citation: Some(format_citation(&cite.chapter, &cite.section)),
            heading: None,
            text: None,
            chars: 0,
            truncated: false,
            repealed: None,
            source: Some(SOURCE.to_string()),
            source_url: Some(section_url(&cite.chapter, &cite.section)),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn live_section_payload_fills_the_response() {
        let c = cite("90/17", None);
        let mut out = blank(&c);
        apply_section(&c, LIVE_17, 50_000, &mut out);
        assert_eq!(out.citation.as_deref(), Some("Mass. Gen. Laws ch. 90, § 17"));
        assert_eq!(out.heading.as_deref(), Some("Speed limits"));
        assert_eq!(out.repealed, Some(false));
        let text = out.text.expect("text present");
        assert!(text.starts_with("Section 17. No person operating"));
        // Paragraph structure survives, and &mdash; is decoded.
        assert!(text.contains("\n\n"), "paragraph break lost: {text:?}");
        assert!(text.contains("posted—it shall"), "entity not decoded: {text:?}");
        assert_eq!(out.chars, text.chars().count());
        assert!(!out.truncated);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn truncation_is_flagged_and_counted_against_the_full_text() {
        let c = cite("90/17", None);
        let mut out = blank(&c);
        apply_section(&c, LIVE_17, 20, &mut out);
        assert_eq!(out.text.as_deref().map(|t| t.chars().count()), Some(20));
        assert!(out.truncated);
        assert!(out.chars > 20, "chars must report the full length, got {}", out.chars);
    }

    #[test]
    fn repealed_sections_are_flagged_and_never_read_as_empty_law() {
        let c = cite("90/50A to 50L", None);
        let mut out = blank(&c);
        apply_section(&c, LIVE_REPEALED, 50_000, &mut out);
        assert_eq!(out.repealed, Some(true));
        // The canonical code from the source, not the one we asked with.
        assert_eq!(
            out.citation.as_deref(),
            Some("Mass. Gen. Laws ch. 90, § 50A to 50L")
        );
        // An empty Text must not surface as "the law says nothing".
        assert!(out.text.is_none());
        assert_eq!(out.chars, 0);
        assert!(
            out.warnings.iter().any(|w| w.contains("REPEALED")),
            "{:?}",
            out.warnings
        );
        assert!(
            out.warnings.iter().any(|w| w.contains("no text")),
            "{:?}",
            out.warnings
        );
    }

    #[test]
    fn a_body_that_is_not_the_expected_json_degrades_to_a_warning() {
        let c = cite("90/17", None);
        let mut out = blank(&c);
        apply_section(&c, "No General Law Sections were found.", 50_000, &mut out);
        assert!(out.text.is_none());
        assert_eq!(out.repealed, None, "unknown is not the same as not-repealed");
        assert_eq!(out.warnings.len(), 1);
        assert!(out.warnings[0].contains("not the expected JSON"), "{:?}", out.warnings);
    }

    #[test]
    fn nearby_codes_are_the_ones_a_typo_meant() {
        let codes: Vec<String> = ["1", "16", "17", "17A", "17B", "17C", "18", "24"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let near = nearby_codes(&codes, "17D");
        for want in ["17", "17A", "17B", "17C"] {
            assert!(near.contains(&want.to_string()), "{want} missing from {near:?}");
        }
        assert!(!near.contains(&"24".to_string()), "{near:?}");
        // Chapter order is preserved so the list reads like the TOC.
        let mut sorted = near.clone();
        sorted.sort_by_key(|c| codes.iter().position(|x| x == c).unwrap_or(usize::MAX));
        assert_eq!(near, sorted);
    }
}
