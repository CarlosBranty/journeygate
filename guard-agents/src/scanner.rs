//! The section-4 diff scanner over a Coder diff (qa-coder-protocol.md step 7;
//! architecture section 4, "Coder rules"). Deterministic and conservative:
//! any finding sends the ticket to the human lane, it never retries.
//!
//! Rules, on application files only (the Coder cannot touch tests anyway):
//!
//! - `test_env_detection`: an **added** line reads the test environment
//!   (`navigator.webdriver`, `process.env.CI`, Playwright variables,
//!   `HeadlessChrome` / user-agent sniffing).
//! - `test_hook_in_logic`: an **added** line branches on a `data-testid`
//!   or on seed values (a condition mentioning `testid` or `seed`).
//! - `removed_guard`: a **removed** line in an auth / permission / payment /
//!   session path that rejected something (`throw`, `401`, `403`, `deny`,
//!   `if (!`, `validate`, `verify`, `require`).

use std::collections::BTreeSet;

pub const TEST_ENV_DETECTION: &str = "test_env_detection";
pub const TEST_HOOK_IN_LOGIC: &str = "test_hook_in_logic";
pub const REMOVED_GUARD: &str = "removed_guard";

const TEST_ENV_MARKERS: &[&str] = &[
    "navigator.webdriver",
    "process.env.ci",
    "env.ci ",
    "playwright",
    "pwtest",
    "headlesschrome",
    "useragent",
    "user-agent",
];

const CONDITION_MARKERS: &[&str] = &[
    "if (", "if(", "===", "!==", "==", "!=", " ? ", "switch (", "case ",
];

const SENSITIVE_PATH_MARKERS: &[&str] = &[
    "auth",
    "login",
    "session",
    "permission",
    "payment",
    "billing",
    "checkout",
];

const GUARD_MARKERS: &[&str] = &[
    "throw",
    "401",
    "403",
    "deny",
    "forbidden",
    "unauthorized",
    "if (!",
    "if(!",
    "validate",
    "verify",
    "require",
];

/// One finding: `"<rule>: <path>: <line>"` (the line trimmed to 160 bytes).
fn finding(rule: &str, path: &str, line: &str) -> String {
    let mut text = line.trim().to_string();
    if text.len() > 160 {
        let mut end = 160;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    format!("{rule}: {path}: {text}")
}

fn is_comment(code: &str) -> bool {
    let c = code.trim_start();
    c.starts_with("//") || c.starts_with('#') || c.starts_with("/*") || c.starts_with('*')
}

/// Findings of `unified_diff`, sorted and deduplicated.
pub fn scan_diff(unified_diff: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    let mut path = String::new();
    for line in unified_diff.lines() {
        if let Some(p) = line.strip_prefix("+++ ") {
            path = p.trim().trim_start_matches("b/").to_string();
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("diff --git") {
            continue;
        }
        if path.is_empty() || path == "/dev/null" {
            continue;
        }
        if let Some(code) = line.strip_prefix('+') {
            if is_comment(code) {
                continue;
            }
            let lower = code.to_ascii_lowercase();
            if TEST_ENV_MARKERS.iter().any(|m| lower.contains(m)) {
                out.insert(finding(TEST_ENV_DETECTION, &path, code));
            }
            let branches = CONDITION_MARKERS.iter().any(|m| lower.contains(m));
            if branches && (lower.contains("testid") || lower.contains("seed")) {
                out.insert(finding(TEST_HOOK_IN_LOGIC, &path, code));
            }
        } else if let Some(code) = line.strip_prefix('-') {
            if is_comment(code) {
                continue;
            }
            let lower_path = path.to_ascii_lowercase();
            let sensitive = SENSITIVE_PATH_MARKERS
                .iter()
                .any(|m| lower_path.contains(m));
            let lower = code.to_ascii_lowercase();
            if sensitive && GUARD_MARKERS.iter().any(|m| lower.contains(m)) {
                out.insert(finding(REMOVED_GUARD, &path, code));
            }
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(path: &str, body: &str) -> String {
        format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,3 @@\n{body}")
    }

    #[test]
    fn a_plain_fix_has_no_findings() {
        let d = diff(
            "src/products/routes.mjs",
            "-  const stock = product.stock > 0 ? `${product.stock} in stock` : 'Out of stock';\n\
             +  const stock = product.stock > 3 ? `${product.stock} in stock` : `Only ${product.stock} left`;\n \
             <p data-testid=\"product-stock\">${stock}</p>\n",
        );
        assert_eq!(scan_diff(&d), Vec::<String>::new());
    }

    #[test]
    fn test_environment_detection_is_found() {
        let d = diff(
            "src/cart/routes.mjs",
            "+  if (navigator.webdriver) return ok();\n+  const ci = process.env.CI;\n",
        );
        let f = scan_diff(&d);
        assert_eq!(f.len(), 2, "{f:?}");
        assert!(
            f.iter()
                .all(|x| x.starts_with("test_env_detection: src/cart/routes.mjs: "))
        );
    }

    #[test]
    fn branching_on_test_ids_or_seeds_is_found() {
        let d = diff(
            "src/products/routes.mjs",
            "+  if (el.dataset.testid === 'product-stock') return;\n+  const x = seed === 42 ? 1 : 2;\n+  const label = 'data-testid';\n",
        );
        let f = scan_diff(&d);
        assert_eq!(f.len(), 2, "{f:?}");
        assert!(f.iter().all(|x| x.starts_with("test_hook_in_logic: ")));
    }

    #[test]
    fn removed_guards_in_sensitive_paths_are_found_but_not_elsewhere() {
        let d = diff(
            "src/checkout/payment.mjs",
            "-  if (!card.valid) throw new StoreError(402, 'declined');\n+  // accepted\n",
        );
        assert_eq!(
            scan_diff(&d),
            vec![
                "removed_guard: src/checkout/payment.mjs: if (!card.valid) throw new StoreError(402, 'declined');"
            ]
        );
        let d = diff(
            "src/products/routes.mjs",
            "-  if (!product) throw new Error('x');\n",
        );
        assert!(scan_diff(&d).is_empty());
    }

    #[test]
    fn comments_and_headers_are_ignored() {
        let d = diff(
            "src/auth/login.mjs",
            "-  // verify later\n+  // navigator.webdriver is not used\n",
        );
        assert!(scan_diff(&d).is_empty());
    }
}
