//! [`journey_change`]: assertion and skip counts of a modified journey spec,
//! from the TypeScript syntax tree (comments and strings never count).

use crate::lang::Grammar;
use guard_core::journey_changes::JourneyChange;
use tree_sitter::{Node, Parser};

/// Facts about one journey spec changed between the base commit and HEAD.
///
/// - `id`: the first `@journey:<id>` tag of `head_src`, else of `base_src` (a
///   removed tag is still reported, under the id it had). `None` when neither
///   version carries a tag.
/// - assertions: `expect(...)`, `expect.poll(...)`, `expect.soft(...)` calls.
/// - skips: calls of any `.skip(` / `.fixme(` member (`test.skip`,
///   `test.describe.fixme`, ...); `skips_added` = after minus before, floored at 0.
///
/// `base_src = None` is a new file (`assertions_before = 0`).
pub fn journey_change(path: &str, base_src: Option<&str>, head_src: &str) -> Option<JourneyChange> {
    let id = journey_tag(head_src).or_else(|| base_src.and_then(journey_tag))?;
    let grammar = match Grammar::for_path(path) {
        Some(g @ (Grammar::TypeScript | Grammar::Tsx | Grammar::JavaScript)) => g,
        _ => Grammar::TypeScript,
    };
    let after = count(grammar, head_src);
    let before = base_src.map(|b| count(grammar, b)).unwrap_or_default();
    Some(JourneyChange {
        id,
        file: path.replace('\\', "/"),
        assertions_before: before.assertions,
        assertions_after: after.assertions,
        skips_added: after.skips.saturating_sub(before.skips),
    })
}

fn journey_tag(src: &str) -> Option<String> {
    const TAG: &str = "@journey:";
    let mut rest = src;
    while let Some(pos) = rest.find(TAG) {
        let tail = &rest[pos + TAG.len()..];
        let id: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .collect();
        let id = id.trim_end_matches('.').to_string();
        if !id.is_empty() {
            return Some(id);
        }
        rest = tail;
    }
    None
}

#[derive(Default, Clone, Copy)]
struct Counts {
    assertions: u32,
    skips: u32,
}

fn count(grammar: Grammar, src: &str) -> Counts {
    let mut parser = Parser::new();
    if parser.set_language(&grammar.ts_language()).is_err() {
        return Counts::default();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Counts::default();
    };
    let bytes = src.as_bytes();
    let mut counts = Counts::default();
    let mut stack: Vec<Node> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "call_expression" {
            if let Some(f) = node.child_by_field_name("function") {
                classify(f, bytes, &mut counts);
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    counts
}

fn classify(function: Node, bytes: &[u8], counts: &mut Counts) {
    let text = |n: Node| n.utf8_text(bytes).unwrap_or("");
    match function.kind() {
        "identifier" if text(function) == "expect" => counts.assertions += 1,
        "member_expression" => {
            let object = function.child_by_field_name("object");
            let property = function
                .child_by_field_name("property")
                .map(text)
                .unwrap_or("");
            let on_expect = object.is_some_and(|o| o.kind() == "identifier" && text(o) == "expect");
            if on_expect && matches!(property, "poll" | "soft") {
                counts.assertions += 1;
            } else if matches!(property, "skip" | "fixme") {
                counts.skips += 1;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
import { test, expect } from "@playwright/test";

test("checkout @journey:checkout.card-payment", async ({ page }) => {
  await expect(page.getByTestId("cart")).toBeVisible();
  await expect.poll(() => page.url()).toContain("/cart");
  expect.soft(1).toBe(1);
  // expect(commented).toBe(out);
  const s = "expect(in a string)";
});
"#;

    const HEAD: &str = r#"
import { test, expect } from "@playwright/test";

test.describe.fixme("flaky", () => {});

test("checkout @journey:checkout.card-payment", async ({ page }) => {
  test.skip(process.env.CI === "1", "later");
  await expect(page.getByTestId("cart")).toBeVisible();
});
"#;

    #[test]
    fn counts_assertions_and_added_skips() {
        let c = journey_change(
            "e2e-playwright/tests/journeys/checkout.spec.ts",
            Some(BASE),
            HEAD,
        )
        .unwrap();
        assert_eq!(c.id, "checkout.card-payment");
        assert_eq!(c.file, "e2e-playwright/tests/journeys/checkout.spec.ts");
        assert_eq!(c.assertions_before, 3);
        assert_eq!(c.assertions_after, 1);
        assert_eq!(c.skips_added, 2);
    }

    #[test]
    fn a_new_file_has_no_assertions_before() {
        let c = journey_change("j.spec.ts", None, BASE).unwrap();
        assert_eq!(c.assertions_before, 0);
        assert_eq!(c.assertions_after, 3);
        assert_eq!(c.skips_added, 0);
    }

    #[test]
    fn removed_skips_never_go_negative() {
        let c = journey_change("j.spec.ts", Some(HEAD), BASE).unwrap();
        assert_eq!(c.skips_added, 0);
        assert_eq!(c.assertions_before, 1);
    }

    #[test]
    fn no_tag_means_not_a_journey() {
        assert_eq!(
            journey_change("a.spec.ts", None, "test('x', () => {});"),
            None
        );
        assert_eq!(journey_tag("@journey: nope"), None);
        assert_eq!(journey_tag("x @journey:a.b. y").as_deref(), Some("a.b"));
    }

    #[test]
    fn a_removed_tag_reports_the_base_id() {
        let head = "test('x', async () => { expect(1).toBe(1); });";
        let c = journey_change("a.spec.ts", Some(BASE), head).unwrap();
        assert_eq!(c.id, "checkout.card-payment");
        assert_eq!(c.assertions_after, 1);
    }
}
