//! Diff path allowlist (qa-coder-protocol.md steps 2 and 7).
//!
//! The paths checked are the union of `changed_paths` and every path named in
//! the unified diff's headers, and the two lists must agree: a diff whose
//! patch touches a file its name list omits is rejected, so the patch applied
//! at finalize can never carry an unchecked path.

use guard_core::ports::{AllowedPaths, ExtractedDiff};
use std::collections::BTreeSet;

/// Why a diff was rejected. `paths` are the offending repo-relative paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRejection {
    pub reason: String,
    pub paths: Vec<String>,
}

impl DiffRejection {
    pub fn feedback(&self) -> String {
        format!(
            "Your change was rejected: {}. Offending paths:\n{}",
            self.reason,
            self.paths
                .iter()
                .map(|p| format!("- {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

/// Paths named by `diff --git`, `--- a/` and `+++ b/` header lines.
/// `Err` = a header it cannot parse (quoted or ambiguous paths).
pub fn paths_in_unified_diff(diff: &str) -> Result<BTreeSet<String>, String> {
    let mut paths = BTreeSet::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if rest.starts_with('"') {
                return Err(format!("quoted diff header: {line}"));
            }
            let rest = rest
                .strip_prefix("a/")
                .ok_or_else(|| format!("unexpected diff header: {line}"))?;
            // `a/<p> b/<p>`: with --no-renames both sides are the same path.
            let half = rest.len().saturating_sub(3) / 2;
            let (Some(a), Some(b)) = (rest.get(..half), rest.get(half..)) else {
                return Err(format!("ambiguous diff header: {line}"));
            };
            if b.strip_prefix(" b/") == Some(a) {
                paths.insert(a.to_string());
            } else {
                return Err(format!("ambiguous diff header: {line}"));
            }
        } else if let Some(p) = line
            .strip_prefix("--- a/")
            .or_else(|| line.strip_prefix("+++ b/"))
        {
            paths.insert(p.trim_end_matches('\t').to_string());
        } else if line.starts_with("--- \"") || line.starts_with("+++ \"") {
            return Err(format!("quoted diff header: {line}"));
        }
    }
    Ok(paths)
}

/// Checks `diff` against `allowed`. `required` paths must be among the changes
/// (QA must write its spec).
pub fn check_diff(
    allowed: &AllowedPaths,
    diff: &ExtractedDiff,
    required: &[String],
) -> Result<(), DiffRejection> {
    let in_patch = paths_in_unified_diff(&diff.unified_diff).map_err(|e| DiffRejection {
        reason: e,
        paths: vec![],
    })?;
    let listed: BTreeSet<String> = diff.changed_paths.iter().cloned().collect();
    let unlisted: Vec<String> = in_patch.difference(&listed).cloned().collect();
    if !unlisted.is_empty() {
        return Err(DiffRejection {
            reason: "the patch touches paths missing from changed_paths".into(),
            paths: unlisted,
        });
    }
    let all: Vec<String> = listed.into_iter().collect();
    let violations = allowed.violations(&all).map_err(|e| DiffRejection {
        reason: format!("invalid allowlist: {e}"),
        paths: vec![],
    })?;
    if !violations.is_empty() {
        return Err(DiffRejection {
            reason: "changes outside the allowed paths".into(),
            paths: violations,
        });
    }
    let missing: Vec<String> = required
        .iter()
        .filter(|r| !all.contains(r))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(DiffRejection {
            reason: "required files were not written".into(),
            paths: missing,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::ports::SealedSpec;

    fn diff(paths: &[&str]) -> ExtractedDiff {
        let unified = paths
            .iter()
            .map(|p| format!("diff --git a/{p} b/{p}\n--- a/{p}\n+++ b/{p}\n@@ -1 +1 @@\n-a\n+b\n"))
            .collect();
        ExtractedDiff {
            base_commit: "abcdef1".into(),
            unified_diff: unified,
            changed_paths: paths.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn coder() -> AllowedPaths {
        let sealed = SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"x");
        AllowedPaths::coder(&["src/**".into()], "tests", &sealed)
    }

    #[test]
    fn coder_may_change_app_paths() {
        assert_eq!(
            check_diff(&coder(), &diff(&["src/cart.ts", "src/a b.ts"]), &[]),
            Ok(())
        );
    }

    #[test]
    fn coder_changes_under_tests_the_sealed_spec_or_outside_are_rejected() {
        for p in [
            "tests/journeys/checkout.spec.ts",
            "tests/features/SHOP-1.spec.ts",
            "package.json",
            "README.md",
            ".github/workflows/ci.yml",
            "playwright.config.ts",
        ] {
            let r = check_diff(&coder(), &diff(&["src/cart.ts", p]), &[]).unwrap_err();
            assert_eq!(r.paths, vec![p.to_string()], "{p}");
            assert!(r.feedback().contains(p));
        }
    }

    #[test]
    fn a_patch_path_missing_from_the_name_list_is_rejected() {
        let mut d = diff(&["src/cart.ts", "tests/smoke.spec.ts"]);
        d.changed_paths = vec!["src/cart.ts".into()];
        let r = check_diff(&coder(), &d, &[]).unwrap_err();
        assert_eq!(r.paths, vec!["tests/smoke.spec.ts".to_string()]);
    }

    #[test]
    fn deleted_and_binary_files_are_parsed() {
        let unified = "diff --git a/src/old.ts b/src/old.ts\ndeleted file mode 100644\n--- a/src/old.ts\n+++ /dev/null\n\
                       diff --git a/tests/x.png b/tests/x.png\nGIT binary patch\nliteral 1\n";
        let paths = paths_in_unified_diff(unified).unwrap();
        assert_eq!(
            paths.into_iter().collect::<Vec<_>>(),
            ["src/old.ts", "tests/x.png"]
        );
        assert!(paths_in_unified_diff("diff --git \"a/x\" \"b/x\"\n").is_err());
    }

    #[test]
    fn qa_must_write_exactly_its_spec() {
        let qa = AllowedPaths::qa("tests", "SHOP-1");
        let spec = "tests/features/SHOP-1.spec.ts".to_string();
        assert_eq!(
            check_diff(&qa, &diff(&[&spec]), std::slice::from_ref(&spec)),
            Ok(())
        );
        let r = check_diff(&qa, &diff(&[]), std::slice::from_ref(&spec)).unwrap_err();
        assert_eq!(r.paths, vec![spec.clone()]);
        let r = check_diff(&qa, &diff(&[&spec, "src/app.ts"]), &[spec]).unwrap_err();
        assert_eq!(r.paths, vec!["src/app.ts".to_string()]);
    }
}
