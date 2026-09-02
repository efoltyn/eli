//! Colorado Revised Statutes.
//!
//! Colorado is a different shape from the other statute states, and a better
//! one. Wisconsin is a per-section HTML fetch and Massachusetts is a JSON API —
//! both leave you dependent on someone's server staying up and staying polite.
//! Colorado publishes the *entire code* as static per-title files (and a single
//! 37 MB zip of all fifty), so a title is fetched once and answered from disk
//! thereafter. No key, no rate limit, no session.
//!
//! The licensing is unusually clean too, which matters if anything is ever built
//! on top of this: under C.R.S. 2-5-118 the Committee on Legal Services
//! suspended both its copyright practice and any fee for the statutory database
//! in March 2016, and expressly permits republication of the official text.
//!
//! Source: https://olls.info/crs/crs{year}-title-{NN}.htm

use super::{StateStatuteRequest, StateStatuteResponse};
use crate::legal::{clamp_text, shared_client, soft_fail};
use crate::{Error, Result};
use std::path::PathBuf;

/// The published edition. Colorado reissues annually; bumping this is the only
/// change a new edition needs.
const CRS_EDITION: u32 = 2026;
/// A title is a static annual publication — a week is conservative.
const CACHE_TTL_SECS: u64 = 60 * 60 * 24 * 7;

pub(super) async fn fetch_statute(req: StateStatuteRequest) -> Result<StateStatuteResponse> {
    let cite = parse_citation(&req.section, req.chapter.as_deref())?;
    let mut out = StateStatuteResponse {
        generated_at: chrono::Utc::now(),
        state: "co".to_string(),
        citation: Some(format!("C.R.S. § {}", cite.section)),
        heading: None,
        text: None,
        chars: 0,
        truncated: false,
        repealed: None,
        source: Some("Colorado Revised Statutes (Office of Legislative Legal Services)".to_string()),
        source_url: Some(title_url(cite.title)),
        warnings: Vec::new(),
    };

    let Some(body) = load_title(cite.title, &mut out.warnings).await else {
        return Ok(out);
    };

    let Some(section) = extract_section(&body, &cite.section) else {
        out.warnings.push(format!(
            "title {} loaded but has no section {}. Check the number — Colorado sections carry the \
             title as their first component, so a section in title {} starts \"{}-\".",
            cite.title, cite.section, cite.title, cite.title
        ));
        return Ok(out);
    };

    out.heading = section.heading;
    out.repealed = Some(section.repealed);
    if section.repealed {
        out.warnings.push(format!(
            "C.R.S. § {} is marked repealed in the {CRS_EDITION} edition — do not rely on it as \
             current law.",
            cite.section
        ));
    }
    out.chars = section.text.chars().count();
    let (clamped, truncated) = clamp_text(&section.text, req.max_chars);
    out.truncated = truncated;
    if truncated {
        out.warnings.push(format!(
            "section text cut to {} of {} chars; raise --max-chars for the rest",
            req.max_chars, out.chars
        ));
    }
    out.text = Some(clamped);
    Ok(out)
}

#[derive(Debug)]
struct Citation {
    title: u32,
    section: String,
}

/// Accept the forms a caller actually types: "13-21-102.5", "§ 13-21-102.5",
/// "C.R.S. 13-21-102.5", or a bare section with `chapter` carrying the title.
///
/// The title is the FIRST component of a Colorado section number, so it is
/// derived rather than asked for — a caller who passes both and disagrees is
/// told, instead of being silently given one of the two.
fn parse_citation(section: &str, chapter: Option<&str>) -> Result<Citation> {
    let cleaned: String = section
        .trim()
        .trim_start_matches("C.R.S.")
        .trim_start_matches("CRS")
        .replace('§', "")
        .trim()
        .to_string();

    let head = cleaned
        .split('-')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();

    // A bare section ("102.5") needs the title from `chapter`.
    if !cleaned.contains('-') {
        let title = chapter
            .and_then(|c| c.trim().parse::<u32>().ok())
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "{section:?} is not a full Colorado citation. Colorado sections look like \
                     13-21-102.5 (title-article-section); pass the whole thing, or give --chapter \
                     with the title number."
                ))
            })?;
        return Ok(Citation {
            title,
            section: format!("{title}-{cleaned}"),
        });
    }

    let title: u32 = head.parse().map_err(|_| {
        Error::InvalidInput(format!(
            "could not read a title number from {section:?}; Colorado sections start with the \
             title, e.g. 13-21-102.5"
        ))
    })?;
    if title == 0 || title > 44 {
        // Colorado's titles run 1-44 with gaps; a number outside that is a
        // transcription error, and fetching it would 404 confusingly.
        return Err(Error::InvalidInput(format!(
            "title {title} is out of range for the Colorado Revised Statutes (titles run 1-44)"
        )));
    }
    if let Some(c) = chapter.and_then(|c| c.trim().parse::<u32>().ok()) {
        if c != title {
            return Err(Error::InvalidInput(format!(
                "--chapter {c} disagrees with the title in {section:?} (title {title}). Colorado \
                 encodes the title in the section number; drop --chapter."
            )));
        }
    }
    Ok(Citation {
        title,
        section: cleaned,
    })
}

fn title_url(title: u32) -> String {
    format!("https://olls.info/crs/crs{CRS_EDITION}-title-{title:02}.htm")
}

fn cache_path(title: u32) -> PathBuf {
    let name = format!("crs{CRS_EDITION}-title-{title:02}.htm");
    directories::ProjectDirs::from("", "", "eli")
        .map(|d| d.cache_dir().join("legal").join("co").join(&name))
        .unwrap_or_else(|| std::env::temp_dir().join(format!("eli-{name}")))
}

fn file_is_fresh(path: &std::path::Path, max_age_secs: u64) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(elapsed) = modified.elapsed() else {
        return false;
    };
    elapsed.as_secs() <= max_age_secs
}

/// A title is 6-8 MB, so it is fetched once and read from disk after that.
/// Without the cache every question would re-download the code.
async fn load_title(title: u32, warnings: &mut Vec<String>) -> Option<String> {
    let path = cache_path(title);
    if file_is_fresh(&path, CACHE_TTL_SECS) {
        if let Ok(s) = std::fs::read_to_string(&path) {
            return Some(s);
        }
    }

    let url = title_url(title);
    let resp = match shared_client::BULK.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("colorado statutes request failed: {e}"));
            return read_stale(&path, warnings);
        }
    };
    let resp = match soft_fail("colorado statutes", resp, warnings).await {
        Some(r) => r,
        None => return read_stale(&path, warnings),
    };
    let body = match resp.text().await {
        Ok(b) => b,
        Err(e) => {
            warnings.push(format!("colorado statutes body read failed: {e}"));
            return read_stale(&path, warnings);
        }
    };

    // A title file is megabytes. Anything small is an error page, not the code —
    // the same 200-with-no-content trap that makes several state statute sites
    // unusable, so check the size before trusting it.
    if body.len() < 100_000 {
        warnings.push(format!(
            "colorado title {title} came back as {} bytes, far too small to be the title text — \
             treating it as an error page rather than statutory text",
            body.len()
        ));
        return read_stale(&path, warnings);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, &body).ok();
    Some(body)
}

/// Serve a stale cached copy rather than nothing when the fetch fails, saying so.
fn read_stale(path: &std::path::Path, warnings: &mut Vec<String>) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    warnings.push(
        "served a previously cached copy of this title because the fetch failed; it may be behind \
         the current edition"
            .to_string(),
    );
    Some(s)
}

struct Section {
    heading: Option<String>,
    text: String,
    repealed: bool,
}

/// Pull one section out of a title file.
///
/// The files are Word exports: no ids, no anchors, just styled `<p>`/`<span>`.
/// A section number appears at least twice — once in the title's table of
/// contents and once at the body — and the body is always last, so the final
/// occurrence is the one to take. Boundaries come from the next section
/// heading rather than any markup.
fn extract_section(body: &str, section: &str) -> Option<Section> {
    let needle = format!(">{section}.");
    let start = find_body_start(body, &needle)?;

    // End at the next section heading of the same title, whatever its number.
    let title_prefix = section.split('-').next().unwrap_or_default();
    let after = &body[start + needle.len()..];
    let end_rel = find_next_heading(after, title_prefix).unwrap_or(after.len());
    let raw = &body[start..start + needle.len() + end_rel];

    let text = flatten(raw);
    let heading = heading_of(&text, section);
    // Colorado marks removed sections in the text itself; there is no flag.
    let repealed = text
        .get(..400)
        .unwrap_or(&text)
        .to_ascii_lowercase()
        .contains("(repealed)");

    Some(Section {
        heading,
        text,
        repealed,
    })
}

/// Last occurrence of the heading that is really THIS section.
///
/// `rfind` alone is wrong, and wrong in the worst way: ">13-21-111." is a
/// prefix of ">13-21-111.8.", so asking for the comparative-negligence statute
/// returned the shooting-range one instead — the right shape of answer about
/// the wrong law. A digit after the trailing period means this is a
/// decimal-suffixed sibling, so keep looking backwards.
fn find_body_start(body: &str, needle: &str) -> Option<usize> {
    let mut end = body.len();
    while let Some(i) = body[..end].rfind(needle) {
        let next = body[i + needle.len()..].chars().next();
        if !next.is_some_and(|c| c.is_ascii_digit()) {
            return Some(i);
        }
        end = i;
    }
    None
}

/// Offset of the next `>NN-` section heading, so a section stops where the
/// following one starts.
fn find_next_heading(haystack: &str, title_prefix: &str) -> Option<usize> {
    let pat = format!(">{title_prefix}-");
    let mut from = 0usize;
    while let Some(i) = haystack[from..].find(&pat) {
        let abs = from + i;
        let tail = &haystack[abs + pat.len()..];
        // Require digits then a period: ">13-21-103." is a heading,
        // ">13-21-103" inside a cross-reference sentence is not.
        let digits: String = tail
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '.')
            .collect();
        if digits.contains('.') && digits.ends_with('.') {
            return Some(abs);
        }
        from = abs + pat.len();
    }
    None
}

/// Strip Word's markup and the non-breaking padding it uses for indentation.
fn flatten(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() / 2);
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    let decoded = html_escape::decode_html_entities(&out);
    let mut collapsed = String::with_capacity(decoded.len());
    let mut last_ws = false;
    for ch in decoded.chars() {
        // U+00A0 and the mojibake Word leaves behind are padding, not content.
        let is_space = ch.is_whitespace() || ch == '\u{a0}' || ch == '\u{fffd}';
        if is_space {
            if !last_ws {
                collapsed.push(' ');
            }
            last_ws = true;
        } else {
            collapsed.push(ch);
            last_ws = false;
        }
    }
    collapsed.trim().to_string()
}

/// The heading is what sits between the section number and the first
/// subsection marker: "13-21-102.5. Limitations on damages ... - definitions."
fn heading_of(text: &str, section: &str) -> Option<String> {
    let after = text.strip_prefix(section)?.trim_start_matches('.').trim();
    let end = after.find("(1)").unwrap_or(after.len().min(200));
    let heading = after[..end].trim().trim_end_matches('.').trim();
    (!heading.is_empty()).then(|| heading.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_citation_forms_a_caller_types() {
        for input in ["13-21-102.5", " § 13-21-102.5", "C.R.S. 13-21-102.5"] {
            let c = parse_citation(input, None).expect(input);
            assert_eq!(c.title, 13, "{input}");
            assert_eq!(c.section, "13-21-102.5", "{input}");
        }
    }

    #[test]
    fn derives_title_from_the_section_number() {
        assert_eq!(parse_citation("42-4-1101", None).expect("parse").title, 42);
        assert_eq!(parse_citation("13-80-102", None).expect("parse").title, 13);
    }

    #[test]
    fn bare_section_needs_a_title_and_says_so() {
        let e = parse_citation("102.5", None).expect_err("must fail");
        assert!(e.to_string().contains("title-article-section"), "{e}");
        let c = parse_citation("102.5", Some("13")).expect("with chapter");
        assert_eq!(c.section, "13-102.5");
    }

    #[test]
    fn contradictory_title_is_rejected_rather_than_guessed() {
        let e = parse_citation("13-21-102.5", Some("42")).expect_err("must fail");
        assert!(e.to_string().contains("disagrees"), "{e}");
    }

    #[test]
    fn rejects_out_of_range_titles() {
        assert!(parse_citation("99-1-1", None).is_err());
    }

    #[test]
    fn builds_zero_padded_title_urls() {
        assert_eq!(title_url(7), format!("https://olls.info/crs/crs{CRS_EDITION}-title-07.htm"));
        assert_eq!(title_url(13), format!("https://olls.info/crs/crs{CRS_EDITION}-title-13.htm"));
    }

    /// The body always follows the table of contents, so the last occurrence
    /// wins. Taking the first returns a one-line index entry as if it were law.
    #[test]
    fn takes_the_body_not_the_table_of_contents() {
        let doc = "<p><span>13-21-102.5.</span> Limitations on damages.</p>\
                   <p><span>13-21-103.</span> Next.</p>\
                   <p><span>13-21-102.5.</span> Limitations on damages - definitions. \
                   (1) The general assembly finds that awards are burdensome.</p>";
        let s = extract_section(doc, "13-21-102.5").expect("found");
        assert!(s.text.contains("general assembly finds"), "{}", s.text);
        assert_eq!(s.heading.as_deref(), Some("Limitations on damages - definitions"));
        assert!(!s.repealed);
    }

    #[test]
    fn stops_at_the_next_section_not_a_cross_reference() {
        let doc = "<p><span>13-21-102.5.</span> Caps. (1) See section 13-21-111 for negligence. \
                   More text here.</p><p><span>13-21-103.</span> Something else entirely.</p>";
        let s = extract_section(doc, "13-21-102.5").expect("found");
        assert!(s.text.contains("See section 13-21-111"), "cross-ref must stay: {}", s.text);
        assert!(!s.text.contains("Something else entirely"), "must stop at next heading");
    }

    /// The bug this cost us: ">13-21-111." prefix-matches ">13-21-111.8.",
    /// so the shooting-range section was returned for a comparative-negligence
    /// query. Right shape, wrong law — the failure mode with no visible tell.
    #[test]
    fn does_not_match_a_decimal_suffixed_sibling() {
        let doc = "<p><span>13-21-111.</span> Negligence cases - comparative negligence.                    (1) Contributory negligence shall not bar recovery.</p>                   <p><span>13-21-111.8.</span> Assumption of risk - shooting ranges.                    (1) Any person who engages in sport shooting.</p>";
        let s = extract_section(doc, "13-21-111").expect("found");
        assert!(s.text.contains("Contributory negligence"), "got: {}", s.text);
        assert!(!s.text.contains("shooting"), "must not return 111.8: {}", s.text);

        // And the sibling is still reachable on its own.
        let s8 = extract_section(doc, "13-21-111.8").expect("found .8");
        assert!(s8.text.contains("shooting"), "got: {}", s8.text);
    }

    #[test]
    fn flags_repealed_sections() {
        let doc = "<p><span>13-21-999.</span> (Repealed) This section was repealed in 2014.</p>";
        let s = extract_section(doc, "13-21-999").expect("found");
        assert!(s.repealed);
    }

    #[test]
    fn flatten_strips_word_padding() {
        assert_eq!(flatten("<p><span>13-1-1.</span>\u{a0}\u{a0} Text&nbsp;here.</p>"), "13-1-1. Text here.");
    }
}
