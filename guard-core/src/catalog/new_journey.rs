//! `guard new journey`: the pure part (cli.md "guard new journey").
//!
//! Builds the catalog entry, appends it to `journeys.json` without touching
//! the bytes of the existing entries, and renders the spec skeleton whose
//! tags match the entry exactly.

use super::{Catalog, JOURNEY_ID, Journey, parse_catalog};
use crate::globs;

/// Why a new journey cannot be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NewJourneyError {
    #[error("journey id {0:?} must match {JOURNEY_ID}")]
    InvalidId(String),
    #[error("--title must not be empty")]
    EmptyTitle,
    #[error("--path {glob:?} is not a valid glob: {reason}")]
    InvalidGlob { glob: String, reason: String },
    #[error("journey {0} is already in the catalog")]
    Duplicate(String),
    #[error("the catalog with the new journey does not validate: {0}")]
    Invalid(String),
}

/// What the user asked for; `None` = the contract default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewJourney {
    pub id: String,
    pub area: Option<String>,
    pub title: Option<String>,
    pub paths: Vec<String>,
    pub smoke: bool,
}

/// Whether `id` matches the catalog id pattern.
pub fn is_valid_id(id: &str) -> bool {
    regex::Regex::new(JOURNEY_ID)
        .expect("built-in pattern compiles")
        .is_match(id)
}

/// The first dot-separated segment of `id`, the default area.
pub fn default_area(id: &str) -> &str {
    id.split_once('.').map_or(id, |(first, _)| first)
}

/// `id` without its first segment, `-` and `.` as spaces, first letter upper-cased.
pub fn default_title(id: &str) -> String {
    let rest = id.split_once('.').map_or(id, |(_, rest)| rest);
    let spaced = rest.replace(['-', '.'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `"This diff can directly or indirectly change how " + lowercase(title)`.
pub fn impact_question(title: &str) -> String {
    format!(
        "This diff can directly or indirectly change how {}",
        title.to_lowercase()
    )
}

/// Validates the request and builds the catalog entry (area not checked
/// against `target.toml` here: the CLI owns the config).
pub fn build(request: &NewJourney) -> Result<Journey, NewJourneyError> {
    if !is_valid_id(&request.id) {
        return Err(NewJourneyError::InvalidId(request.id.clone()));
    }
    let title = match &request.title {
        Some(title) => title.trim().to_string(),
        None => default_title(&request.id),
    };
    if title.is_empty() {
        return Err(NewJourneyError::EmptyTitle);
    }
    for glob in &request.paths {
        globs::compile(glob).map_err(|e| NewJourneyError::InvalidGlob {
            glob: glob.clone(),
            reason: e.kind().to_string(),
        })?;
    }
    Ok(Journey {
        area: request
            .area
            .clone()
            .unwrap_or_else(|| default_area(&request.id).to_string()),
        impact_question: impact_question(&title),
        title,
        id: request.id.clone(),
        paths: request.paths.clone(),
        smoke: request.smoke,
        quarantined: false,
    })
}

fn json(value: &str) -> String {
    serde_json::to_string(value).expect("a string always serializes")
}

/// One journey object, indented for the `journeys` array (two spaces per level).
fn render_entry(journey: &Journey) -> String {
    let paths: Vec<String> = journey.paths.iter().map(|p| json(p)).collect();
    format!(
        "    {{\n      \"id\": {},\n      \"area\": {},\n      \"title\": {},\n      \"impact_question\": {},\n      \"paths\": [{}],\n      \"smoke\": {},\n      \"quarantined\": {}\n    }}",
        json(&journey.id),
        json(&journey.area),
        json(&journey.title),
        json(&journey.impact_question),
        paths.join(", "),
        journey.smoke,
        journey.quarantined,
    )
}

/// The whole catalog in the canonical layout: two-space indent, `paths` on one
/// line, trailing newline.
pub fn render_catalog(catalog: &Catalog) -> String {
    let mut out = String::from("{\n");
    if let Some(schema) = &catalog.schema {
        out.push_str(&format!("  \"$schema\": {},\n", json(schema)));
    }
    out.push_str(&format!("  \"version\": {},\n", catalog.version));
    if catalog.journeys.is_empty() {
        out.push_str("  \"journeys\": []\n}\n");
    } else {
        let entries: Vec<String> = catalog.journeys.iter().map(render_entry).collect();
        out.push_str(&format!(
            "  \"journeys\": [\n{}\n  ]\n}}\n",
            entries.join(",\n")
        ));
    }
    out
}

/// Inserts `entry` before the closing `]` of a trailing `journeys` array,
/// leaving every other byte as it was. `None` when the text has another shape.
fn insert_textually(text: &str, entry: &str) -> Option<String> {
    let body = text.trim_end();
    let before_brace = body.strip_suffix('}')?.trim_end();
    let close = before_brace.len().checked_sub(1)?;
    if !before_brace.ends_with(']') {
        return None;
    }
    let head = before_brace[..close].trim_end();
    let tail = &text[close..];
    let joined = if head.ends_with('[') {
        format!("{head}\n{entry}\n  {tail}")
    } else if head.ends_with('}') {
        format!("{head},\n{entry}\n  {tail}")
    } else {
        return None;
    };
    Some(joined)
}

/// `text` (a valid catalog) with `journey` appended to `journeys[]`.
///
/// The existing bytes are kept when the array is the file's last key;
/// otherwise the whole file is rewritten in the canonical layout. Either
/// way the result is re-parsed and must hold exactly the old journeys plus
/// the new one.
pub fn append_journey(text: &str, journey: &Journey) -> Result<String, NewJourneyError> {
    let catalog = parse_catalog(text).map_err(|e| NewJourneyError::Invalid(e.to_string()))?;
    if catalog.find(&journey.id).is_some() {
        return Err(NewJourneyError::Duplicate(journey.id.clone()));
    }
    let mut expected = catalog.clone();
    expected.journeys.push(journey.clone());
    let candidate = insert_textually(text, &render_entry(journey))
        .filter(|joined| parse_catalog(joined).is_ok_and(|parsed| parsed == expected))
        .unwrap_or_else(|| render_catalog(&expected));
    match parse_catalog(&candidate) {
        Ok(parsed) if parsed == expected => Ok(candidate),
        Ok(_) => Err(NewJourneyError::Invalid(
            "the rewritten catalog lost an entry".into(),
        )),
        Err(e) => Err(NewJourneyError::Invalid(e.to_string())),
    }
}

/// `tests/journeys/<area>/<id without the first segment>.spec.ts`, forward slashes.
pub fn spec_path(journey: &Journey) -> String {
    let rest = journey
        .id
        .split_once('.')
        .map_or(journey.id.as_str(), |(_, rest)| rest);
    format!("tests/journeys/{}/{rest}.spec.ts", journey.area)
}

/// The catalog tags of `journey`: `@journey:<id>`, `@area:<area>`, then
/// `@smoke` iff the entry is a smoke journey.
pub fn tags(journey: &Journey) -> Vec<String> {
    let mut tags = vec![journey.tag(), journey.area_tag()];
    if journey.smoke {
        tags.push("@smoke".into());
    }
    tags
}

/// A TypeScript single-quoted string literal.
fn ts(value: &str) -> String {
    let mut out = String::from("'");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// The spec skeleton: one `test.describe` with exactly the catalog tags and
/// one `test.fixme` to fill in.
pub fn render_spec(journey: &Journey) -> String {
    let tags: Vec<String> = tags(journey).iter().map(|t| ts(t)).collect();
    format!(
        "import {{ expect, test }} from '../../../src/fixtures';\n\
         \n\
         test.describe({title}, {{ tag: [{tags}] }}, () => {{\n\
         \x20 // Fill in the journey, then turn test.fixme into test.\n\
         \x20 test.fixme('works end to end', async ({{ page }}) => {{\n\
         \x20   await page.goto('/');\n\
         \x20   await expect(page).toHaveTitle(/\\S/);\n\
         \x20 }});\n\
         }});\n",
        title = ts(&journey.title),
        tags = tags.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str) -> NewJourney {
        NewJourney {
            id: id.into(),
            ..NewJourney::default()
        }
    }

    const DOGFOOD: &str = r#"{
  "$schema": "../../schemas/journeys.schema.json",
  "version": 1,
  "journeys": [
    {
      "id": "checkout.customer-card",
      "area": "checkout",
      "title": "Customer pays by card",
      "impact_question": "This diff can directly or indirectly change how a customer pays.",
      "paths": ["web/src/checkout/**", "web/src/cart/**"],
      "smoke": true,
      "quarantined": false
    }
  ]
}
"#;

    #[test]
    fn defaults_follow_the_contract() {
        let journey = build(&request("checkout.customer-card.retry")).unwrap();
        assert_eq!(journey.area, "checkout");
        assert_eq!(journey.title, "Customer card retry");
        assert_eq!(
            journey.impact_question,
            "This diff can directly or indirectly change how customer card retry"
        );
        assert_eq!(journey.paths, Vec::<String>::new());
        assert!(!journey.smoke && !journey.quarantined);
    }

    #[test]
    fn explicit_values_win_and_the_title_is_lowercased_in_the_question() {
        let journey = build(&NewJourney {
            id: "billing.invoice".into(),
            area: Some("payments".into()),
            title: Some("  Admin Issues An Invoice ".into()),
            paths: vec!["billing/**".into()],
            smoke: true,
        })
        .unwrap();
        assert_eq!(journey.area, "payments");
        assert_eq!(journey.title, "Admin Issues An Invoice");
        assert_eq!(
            journey.impact_question,
            "This diff can directly or indirectly change how admin issues an invoice"
        );
        assert!(journey.smoke);
    }

    #[test]
    fn invalid_ids_titles_and_globs_are_rejected() {
        for id in ["checkout", "Checkout.card", "checkout.card_x", "a..b", ""] {
            assert_eq!(
                build(&request(id)),
                Err(NewJourneyError::InvalidId(id.into())),
                "{id}"
            );
        }
        let mut empty_title = request("a.b");
        empty_title.title = Some("   ".into());
        assert_eq!(build(&empty_title), Err(NewJourneyError::EmptyTitle));
        let mut bad_glob = request("a.b");
        bad_glob.paths = vec!["web/[".into()];
        assert!(matches!(
            build(&bad_glob),
            Err(NewJourneyError::InvalidGlob { .. })
        ));
    }

    #[test]
    fn appending_keeps_existing_bytes_and_adds_a_canonical_entry() {
        let journey = build(&NewJourney {
            id: "checkout.refund".into(),
            paths: vec!["web/src/refund/**".into()],
            ..NewJourney::default()
        })
        .unwrap();
        let out = append_journey(DOGFOOD, &journey).unwrap();
        let kept = DOGFOOD.rsplit_once("\n  ]").unwrap().0;
        assert!(out.starts_with(kept), "{out}");
        assert_eq!(
            &out[kept.len()..],
            r#",
    {
      "id": "checkout.refund",
      "area": "checkout",
      "title": "Refund",
      "impact_question": "This diff can directly or indirectly change how refund",
      "paths": ["web/src/refund/**"],
      "smoke": false,
      "quarantined": false
    }
  ]
}
"#
        );
        let parsed = parse_catalog(&out).unwrap();
        assert_eq!(parsed.journeys.len(), 2);
        assert_eq!(parsed.journeys[1], journey);
    }

    #[test]
    fn appending_to_an_empty_array_and_twice_stays_valid() {
        let empty = "{\n  \"version\": 1,\n  \"journeys\": []\n}\n";
        let a = build(&request("home.landing")).unwrap();
        let b = build(&request("home.search")).unwrap();
        let once = append_journey(empty, &a).unwrap();
        assert_eq!(
            once,
            render_catalog(&Catalog {
                schema: None,
                version: 1,
                journeys: vec![a.clone()],
            })
        );
        let twice = append_journey(&once, &b).unwrap();
        assert_eq!(
            twice,
            render_catalog(&Catalog {
                schema: None,
                version: 1,
                journeys: vec![a, b],
            })
        );
    }

    #[test]
    fn an_unusual_layout_is_rewritten_canonically() {
        let compact = r#"{"journeys":[],"version":1}"#;
        let a = build(&request("home.landing")).unwrap();
        let out = append_journey(compact, &a).unwrap();
        assert_eq!(
            out,
            render_catalog(&Catalog {
                schema: None,
                version: 1,
                journeys: vec![a],
            })
        );
    }

    #[test]
    fn duplicates_are_refused() {
        let journey = build(&request("checkout.customer-card")).unwrap();
        assert_eq!(
            append_journey(DOGFOOD, &journey),
            Err(NewJourneyError::Duplicate("checkout.customer-card".into()))
        );
    }

    #[test]
    fn titles_with_quotes_are_escaped_in_json() {
        let mut req = request("home.quote");
        req.title = Some(r#"Says "hi" \ there"#.into());
        let journey = build(&req).unwrap();
        let out = append_journey("{\"version\":1,\"journeys\":[]}", &journey).unwrap();
        assert_eq!(parse_catalog(&out).unwrap().journeys[0], journey);
    }

    #[test]
    fn spec_path_drops_only_the_first_segment() {
        let mut journey = build(&request("checkout.customer-card")).unwrap();
        assert_eq!(
            spec_path(&journey),
            "tests/journeys/checkout/customer-card.spec.ts"
        );
        journey = build(&NewJourney {
            id: "checkout.card.retry".into(),
            area: Some("payments".into()),
            ..NewJourney::default()
        })
        .unwrap();
        assert_eq!(
            spec_path(&journey),
            "tests/journeys/payments/card.retry.spec.ts"
        );
    }

    #[test]
    fn spec_carries_exactly_the_catalog_tags() {
        let plain = build(&request("checkout.refund")).unwrap();
        assert_eq!(
            render_spec(&plain),
            "import { expect, test } from '../../../src/fixtures';\n\
             \n\
             test.describe('Refund', { tag: ['@journey:checkout.refund', '@area:checkout'] }, () => {\n\
             \x20 // Fill in the journey, then turn test.fixme into test.\n\
             \x20 test.fixme('works end to end', async ({ page }) => {\n\
             \x20   await page.goto('/');\n\
             \x20   await expect(page).toHaveTitle(/\\S/);\n\
             \x20 });\n\
             });\n"
        );
        let mut req = request("checkout.refund");
        req.smoke = true;
        req.title = Some("Customer's refund".into());
        let smoke = render_spec(&build(&req).unwrap());
        assert!(
            smoke.contains(
                "test.describe('Customer\\'s refund', { tag: ['@journey:checkout.refund', '@area:checkout', '@smoke'] }"
            ),
            "{smoke}"
        );
    }
}
