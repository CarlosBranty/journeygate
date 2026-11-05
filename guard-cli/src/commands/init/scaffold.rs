//! Files `guard init` writes besides `target.toml`, embedded in the binary:
//! the generic Playwright project of `templates/e2e-playwright/`, the CI
//! pipelines and the pre-push hook.

/// `templates/e2e-playwright/`, by path relative to the Playwright dir.
pub const E2E_TEMPLATE: &[(&str, &str)] = &[
    (
        ".gitignore",
        include_str!("../../../../templates/e2e-playwright/.gitignore"),
    ),
    (
        ".npmrc",
        include_str!("../../../../templates/e2e-playwright/.npmrc"),
    ),
    (
        "AGENTS.md",
        include_str!("../../../../templates/e2e-playwright/AGENTS.md"),
    ),
    (
        "catalog/journeys.json",
        include_str!("../../../../templates/e2e-playwright/catalog/journeys.json"),
    ),
    (
        "eslint.config.mjs",
        include_str!("../../../../templates/e2e-playwright/eslint.config.mjs"),
    ),
    (
        "package.json",
        include_str!("../../../../templates/e2e-playwright/package.json"),
    ),
    (
        "playwright.config.ts",
        include_str!("../../../../templates/e2e-playwright/playwright.config.ts"),
    ),
    (
        "scripts/catalog-lint.ts",
        include_str!("../../../../templates/e2e-playwright/scripts/catalog-lint.ts"),
    ),
    (
        "scripts/lib/catalog-lint-run.ts",
        include_str!("../../../../templates/e2e-playwright/scripts/lib/catalog-lint-run.ts"),
    ),
    (
        "scripts/lib/catalog-rules.ts",
        include_str!("../../../../templates/e2e-playwright/scripts/lib/catalog-rules.ts"),
    ),
    (
        "src/config/env.ts",
        include_str!("../../../../templates/e2e-playwright/src/config/env.ts"),
    ),
    (
        "src/fixtures/auth.ts",
        include_str!("../../../../templates/e2e-playwright/src/fixtures/auth.ts"),
    ),
    (
        "src/fixtures/blocked-domains.ts",
        include_str!("../../../../templates/e2e-playwright/src/fixtures/blocked-domains.ts"),
    ),
    (
        "src/fixtures/data.ts",
        include_str!("../../../../templates/e2e-playwright/src/fixtures/data.ts"),
    ),
    (
        "src/fixtures/index.ts",
        include_str!("../../../../templates/e2e-playwright/src/fixtures/index.ts"),
    ),
    (
        "src/fixtures/namespace.ts",
        include_str!("../../../../templates/e2e-playwright/src/fixtures/namespace.ts"),
    ),
    (
        "src/fixtures/seeded-random.ts",
        include_str!("../../../../templates/e2e-playwright/src/fixtures/seeded-random.ts"),
    ),
    (
        "tests/setup/auth.setup.ts",
        include_str!("../../../../templates/e2e-playwright/tests/setup/auth.setup.ts"),
    ),
    (
        "tests/smoke/home.spec.ts",
        include_str!("../../../../templates/e2e-playwright/tests/smoke/home.spec.ts"),
    ),
    (
        "tsconfig.json",
        include_str!("../../../../templates/e2e-playwright/tsconfig.json"),
    ),
];

/// `docs/getting-started.md` section 7.3 with `<action-ref>` in each `uses:`.
const GITHUB_WORKFLOW: &str = include_str!("../../../../templates/ci/github-guard.yml");

/// The GitLab component, used as the whole `.gitlab-ci.yml`.
pub const GITLAB_CI: &str = include_str!("../../../../ci/gitlab/templates/gate.yml");

pub const PRE_PUSH_HOOK: &str = "#!/bin/sh\nexec guard test\n";

/// Embedded files keep LF line endings whatever the checkout did.
pub fn lf(text: &str) -> String {
    text.replace("\r\n", "\n")
}

pub fn github_workflow(action_ref: &str) -> String {
    lf(GITHUB_WORKFLOW).replace("<action-ref>", action_ref)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Template files that must stay byte-identical to the reference project.
    const SHARED: &[(&str, &str)] = &[
        (
            "playwright.config.ts",
            include_str!("../../../../e2e-playwright/playwright.config.ts"),
        ),
        (".npmrc", include_str!("../../../../e2e-playwright/.npmrc")),
        (
            "src/config/env.ts",
            include_str!("../../../../e2e-playwright/src/config/env.ts"),
        ),
        (
            "src/fixtures/auth.ts",
            include_str!("../../../../e2e-playwright/src/fixtures/auth.ts"),
        ),
        (
            "src/fixtures/blocked-domains.ts",
            include_str!("../../../../e2e-playwright/src/fixtures/blocked-domains.ts"),
        ),
        (
            "src/fixtures/namespace.ts",
            include_str!("../../../../e2e-playwright/src/fixtures/namespace.ts"),
        ),
        (
            "src/fixtures/seeded-random.ts",
            include_str!("../../../../e2e-playwright/src/fixtures/seeded-random.ts"),
        ),
        (
            "tests/setup/auth.setup.ts",
            include_str!("../../../../e2e-playwright/tests/setup/auth.setup.ts"),
        ),
        (
            "scripts/catalog-lint.ts",
            include_str!("../../../../e2e-playwright/scripts/catalog-lint.ts"),
        ),
        (
            "scripts/lib/catalog-lint-run.ts",
            include_str!("../../../../e2e-playwright/scripts/lib/catalog-lint-run.ts"),
        ),
        (
            "scripts/lib/catalog-rules.ts",
            include_str!("../../../../e2e-playwright/scripts/lib/catalog-rules.ts"),
        ),
    ];

    fn template(path: &str) -> String {
        let (_, body) = E2E_TEMPLATE
            .iter()
            .find(|(p, _)| *p == path)
            .unwrap_or_else(|| panic!("{path} is not in the template"));
        lf(body)
    }

    #[test]
    fn the_template_is_sorted_and_unique() {
        let paths: Vec<&str> = E2E_TEMPLATE.iter().map(|(p, _)| *p).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(paths, sorted);
    }

    #[test]
    fn shared_files_match_the_reference_project() {
        for (path, reference) in SHARED {
            assert_eq!(template(path), lf(reference), "{path} drifted");
        }
    }

    #[test]
    fn package_json_pins_the_reference_versions() {
        let deps = |text: &str| -> BTreeMap<String, String> {
            let pkg: serde_json::Value = serde_json::from_str(text).unwrap();
            pkg["devDependencies"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                .collect()
        };
        let ours = deps(&template("package.json"));
        let reference = deps(include_str!("../../../../e2e-playwright/package.json"));
        assert!(!ours.is_empty());
        for (name, version) in &ours {
            assert_eq!(reference.get(name), Some(version), "{name}");
            assert!(
                version.chars().all(|c| c.is_ascii_digit() || c == '.'),
                "{name} {version} is not pinned"
            );
        }
    }

    #[test]
    fn the_catalog_is_the_empty_catalog() {
        assert_eq!(
            template("catalog/journeys.json"),
            guard_core::detect::EMPTY_CATALOG
        );
    }

    #[test]
    fn scaffold_honours_the_login_and_seed_path_variables() {
        assert!(template("tests/setup/auth.setup.ts").contains("GUARD_LOGIN_PATH"));
        assert!(template("src/fixtures/index.ts").contains("GUARD_SEED_PATH"));
        assert!(template("tests/smoke/home.spec.ts").contains("@smoke"));
    }

    #[test]
    fn the_github_workflow_is_the_getting_started_workflow() {
        let guide = lf(include_str!("../../../../docs/getting-started.md"));
        let section = &guide[guide.find("### 7.3 The workflow").unwrap()..];
        let start = section.find("```yaml\n").unwrap() + "```yaml\n".len();
        let end = start + section[start..].find("```\n").unwrap();
        let documented = &section[start..end];
        assert_eq!(
            github_workflow("<owner>/e2e-agentdev/ci/github-action@main"),
            documented
        );
        let ours = github_workflow("acme/x/ci/github-action@v1");
        assert_eq!(ours.matches("uses: acme/x/ci/github-action@v1").count(), 3);
    }

    /// The jobs that run PR code (`plan`, `shard`) get neither write
    /// permissions nor the test token they do not need.
    #[test]
    fn the_github_workflow_scopes_permissions_and_the_test_token() {
        let workflow = github_workflow("acme/x/ci/github-action@v1");
        let jobs = workflow.find("\njobs:\n").unwrap();
        let top = &workflow[..jobs];
        assert!(top.contains("permissions:\n  contents: read\n"), "{top}");
        assert!(!top.contains("write"), "no workflow-level write: {top}");
        assert!(
            !top.contains("GUARD_TEST_TOKEN"),
            "no workflow-level token: {top}"
        );
        // A job is its `  <name>:` line up to the next line at that indentation.
        let job = |name: &str| {
            let header = format!("  {name}:");
            let is_job = |l: &str| {
                l.starts_with("  ")
                    && !l.starts_with("   ")
                    && l.trim_end().ends_with(':')
                    && !l.trim_start().starts_with('#')
            };
            workflow[jobs..]
                .lines()
                .skip_while(|l| *l != header)
                .enumerate()
                .take_while(|(i, l)| *i == 0 || !is_job(l))
                .map(|(_, l)| format!("{l}\n"))
                .collect::<String>()
        };
        let plan = job("plan");
        assert!(
            !plan.contains("write") && !plan.contains("GUARD_TEST_TOKEN"),
            "{plan}"
        );
        let shard = job("shard");
        assert!(!shard.contains("write"), "{shard}");
        assert_eq!(shard.matches("GUARD_TEST_TOKEN:").count(), 1, "{shard}");
        let finalize = job("finalize");
        assert!(finalize.contains("checks: write"), "{finalize}");
        assert!(finalize.contains("pull-requests: write"), "{finalize}");
    }
}
