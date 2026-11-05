//! Demo data for the screenshot harness when no `--repo` is given: a fake
//! project held in memory only. Nothing is read from disk or run.

use super::{ALL_FAILURES, ExplainState, GuardApp, Project};
use crate::args::ShotView;
use crate::catalog::JourneyRow;
use crate::projects::Validation;
use crate::results::{JourneyResult, Outcome};
use crate::runner::Exit;
use crate::safety::Safety;
use crate::status::{Affected, StatusEntry, StatusReport};
use crate::suspects::{Blame, JourneySuspects, Suspect, SuspectsDoc};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const JOURNEYS: &[(&str, &str, &str, bool, bool)] = &[
    (
        "account.login",
        "account",
        "Customer signs in with email and password",
        true,
        false,
    ),
    (
        "account.reset-password",
        "account",
        "Customer resets a forgotten password",
        false,
        false,
    ),
    (
        "account.addresses",
        "account",
        "Customer adds and edits a delivery address",
        false,
        false,
    ),
    (
        "catalog.search",
        "catalog",
        "Search returns products for a keyword",
        true,
        false,
    ),
    (
        "catalog.filters",
        "catalog",
        "Filters narrow the product list by brand and price",
        false,
        false,
    ),
    (
        "catalog.product-page",
        "catalog",
        "Product page shows price, stock and gallery",
        false,
        false,
    ),
    (
        "catalog.compare",
        "catalog",
        "Customer compares two products side by side",
        false,
        true,
    ),
    (
        "checkout.customer-card",
        "checkout",
        "Customer pays by card and gets one confirmed order",
        true,
        false,
    ),
    (
        "checkout.coupon",
        "checkout",
        "A valid coupon lowers the order total",
        false,
        false,
    ),
    (
        "checkout.store-pickup",
        "checkout",
        "Customer picks the order up in a store",
        false,
        false,
    ),
    (
        "checkout.split-payment",
        "checkout",
        "Order paid with a gift card and a credit card",
        false,
        false,
    ),
    (
        "checkout.guest",
        "checkout",
        "Guest checkout without an account",
        false,
        false,
    ),
    (
        "billing.invoice-pdf",
        "billing",
        "Invoice PDF is issued after a paid order",
        false,
        true,
    ),
    (
        "billing.credit-note",
        "billing",
        "A refund issues a credit note",
        false,
        false,
    ),
    (
        "intranet.orders-board",
        "intranet",
        "Staff see new orders on the intranet board",
        false,
        false,
    ),
    (
        "intranet.stock-adjust",
        "intranet",
        "Staff adjust stock and the store reflects it",
        false,
        false,
    ),
];

fn result(outcome: Outcome, ms: u64, error: Option<&str>) -> JourneyResult {
    JourneyResult {
        outcome,
        duration_ms: ms,
        tests: 1,
        error: error.map(str::to_string),
        attachments: Vec::new(),
        files: Vec::new(),
    }
}

impl GuardApp {
    pub(super) fn load_demo(&mut self, view: ShotView) {
        let root = PathBuf::from(if cfg!(windows) {
            r"C:\work\demo-shop"
        } else {
            "/work/demo-shop"
        });
        self.project = Some(Project {
            name: "demo-shop".into(),
            root,
            validation: Validation {
                missing: Vec::new(),
            },
        });
        self.branch = Some("feat/checkout-split-payment".into());
        self.env_names = ["E2E_BASE_URL", "E2E_PASSWORD", "E2E_USER"]
            .map(String::from)
            .to_vec();
        self.rows = JOURNEYS
            .iter()
            .map(|(id, area, title, smoke, quarantined)| JourneyRow {
                id: (*id).into(),
                area: (*area).into(),
                title: (*title).into(),
                paths: vec![format!("web/src/{area}/**"), format!("services/{area}/**")],
                smoke: *smoke,
                quarantined: *quarantined,
                safety: Safety::Unclassified,
            })
            .collect();
        let failure = "Error: expect(locator).toHaveText(expected)\n\nLocator: getByTestId('order-total')\nExpected: \"$ 84.90\"\nReceived: \"$ 94.90\"\n\n  at tests/journeys/checkout/split-payment.spec.ts:42:51";
        let results = [
            ("account.login", result(Outcome::Passed, 4_210, None)),
            (
                "account.reset-password",
                result(Outcome::Passed, 6_840, None),
            ),
            ("account.addresses", result(Outcome::Passed, 5_120, None)),
            ("catalog.search", result(Outcome::Passed, 3_330, None)),
            (
                "catalog.filters",
                result(
                    Outcome::Flaky,
                    9_870,
                    Some(
                        "Timeout 5000ms exceeded waiting for getByRole('checkbox', { name: 'Brand' })",
                    ),
                ),
            ),
            ("catalog.product-page", result(Outcome::Passed, 2_950, None)),
            (
                "checkout.customer-card",
                result(Outcome::Passed, 12_400, None),
            ),
            ("checkout.coupon", result(Outcome::Passed, 8_010, None)),
            (
                "checkout.split-payment",
                result(Outcome::Failed, 14_730, Some(failure)),
            ),
            (
                "checkout.guest",
                result(
                    Outcome::Failed,
                    11_020,
                    Some(
                        "Error: page.goto: net::ERR_CONNECTION_REFUSED at http://localhost:5173/checkout",
                    ),
                ),
            ),
            ("billing.credit-note", result(Outcome::Skipped, 0, None)),
            (
                "intranet.orders-board",
                result(Outcome::Passed, 7_480, None),
            ),
        ];
        self.results = results
            .into_iter()
            .map(|(id, r)| (id.to_string(), r))
            .collect();
        self.affected = Some(Ok(Affected {
            base: "origin/main".into(),
            changed_files: 7,
            tags: Vec::new(),
            ids: [
                "checkout.customer-card",
                "checkout.split-payment",
                "checkout.store-pickup",
                "checkout.guest",
                "intranet.stock-adjust",
            ]
            .map(String::from)
            .to_vec(),
        }));
        let entry = |kind: &str, name: &str, pid: u32, ready: bool, url: &str| StatusEntry {
            kind: kind.into(),
            name: name.into(),
            cwd: ".".into(),
            start: None,
            ready_url: Some(url.into()),
            pid: Some(pid),
            alive: Some(true),
            ready: Some(ready),
            log: format!(".guard/logs/{name}.log"),
        };
        self.status = Some(Ok(StatusReport {
            base_url: "http://localhost:5173".into(),
            ready: true,
            entries: vec![
                entry("app", "web", 18_244, true, "http://localhost:5173"),
                entry(
                    "service",
                    "api",
                    18_102,
                    true,
                    "http://localhost:8080/health",
                ),
                entry(
                    "service",
                    "intranet",
                    18_377,
                    true,
                    "http://localhost:8090/health",
                ),
            ],
        }));
        for line in [
            "$ guard test --affected",
            "affected vs origin/main: 5 journeys from 7 changed files (+ smoke)",
            "Running 8 tests using 2 workers",
            "  \u{2713}  1 [chromium] > account/login.spec.ts > Customer signs in (4.2s)",
            "  \u{2713}  2 [chromium] > catalog/search.spec.ts > Search returns products (3.3s)",
            "  \u{2713}  3 [chromium] > checkout/customer-card.spec.ts > Customer pays by card (12.4s)",
            "  \u{2718}  4 [chromium] > checkout/split-payment.spec.ts > Gift card and credit card (14.7s)",
            "  \u{2718}  5 [chromium] > checkout/guest.spec.ts > Guest checkout (11.0s)",
            "  -  6 [chromium] > checkout/store-pickup.spec.ts > Store pickup",
            "  \u{2713}  7 [chromium] > intranet/orders-board.spec.ts > Orders board (7.5s)",
            "",
            "  1) [chromium] > checkout/split-payment.spec.ts:42:51 > Gift card and credit card",
            "    Error: expect(locator).toHaveText(expected)",
            "    Expected: \"$ 84.90\"",
            "    Received: \"$ 94.90\"",
            "",
            "  2 failed",
            "  1 skipped",
            "  5 passed (52.6s)",
            "[exit 1]",
        ] {
            self.log.push(line.to_string());
        }
        self.last_exit = Some(("guard test --affected".into(), Exit::Code(1)));
        self.selected = ["checkout.split-payment", "checkout.guest"]
            .map(String::from)
            .into();
        self.focused = Some("checkout.split-payment".into());
        // Canned AI Summaries (in Spanish, the default language) so the
        // harness can screenshot the panels without any API call.
        self.explain.insert(
            "checkout.split-payment".into(),
            ExplainState::Done(DEMO_JOURNEY_SUMMARY.into()),
        );
        if view == ShotView::Problems {
            self.explain.insert(
                ALL_FAILURES.into(),
                ExplainState::Done(DEMO_ALL_SUMMARY.into()),
            );
        }
        // Canned `guard suspects` rows; no git runs. The stamp matches the
        // demo's (absent) report, so nothing is fetched.
        self.suspects = Some((self.report_stamp, Ok(demo_suspects())));
        // The settings shot shows the saved-key state; no store is read.
        self.ai_key_saved = view == ShotView::AiSettings;
        // ...and a fetched model list; the harness never runs guard for it.
        if view == ShotView::AiSettings {
            let key = crate::models::FetchKey::of(&self.ai, self.ai_key_saved);
            self.models.started(&key);
            self.models.finished(
                &key,
                Ok(DEMO_MODELS
                    .iter()
                    .map(|(id, name)| crate::models::ModelInfo {
                        id: (*id).into(),
                        name: Some((*name).into()),
                    })
                    .collect()),
            );
        }
        if view == ShotView::Flow {
            self.load_demo_integrations();
        }
    }

    /// A mixed integrations report (Jira connected, guard serve and its
    /// gatekeeper not configured, one agent task running, CI not set up yet)
    /// and a local test run halfway through; nothing runs or is read.
    fn load_demo_integrations(&mut self) {
        self.integrations = Some(crate::integrations::parse(DEMO_INTEGRATIONS));
        self.integrations_at =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(12));
        self.integration_settings = crate::integrations::IntegrationSettings {
            jira_base_url: "https://acme.atlassian.net".into(),
            jira_email: "qa@acme.com".into(),
            gateway_url: String::new(),
        };
        self.jira_base_url_input = self.integration_settings.jira_base_url.clone();
        self.jira_email_input = self.integration_settings.jira_email.clone();
        self.gateway_url_input.clear();
        self.jira_token_saved = true;
        self.jira_project_input = "SHOP".into();
        for line in [
            "Running 8 tests using 2 workers",
            "  \u{2713}  1 [chromium] > account/login.spec.ts > Customer signs in (4.2s)",
            "  \u{2713}  2 [chromium] > catalog/search.spec.ts > Search returns products (3.3s)",
            "  \u{2713}  3 [chromium] > checkout/customer-card.spec.ts > Customer pays by card (12.4s)",
            "  \u{2718}  4 [chromium] > checkout/split-payment.spec.ts > Gift card and credit card (14.7s)",
        ] {
            self.run_progress.feed(line);
        }
    }
}

const DEMO_INTEGRATIONS: &str = r#"{"integrations": [
  {"id": "jira", "nodes": ["jira"], "label": "Jira", "state": "connected",
   "detail": "authenticated as Ana QA at acme.atlassian.net; project SHOP"},
  {"id": "server", "nodes": ["ingest"], "label": "guard serve", "state": "not_configured",
   "detail": "no [server] url in target.toml",
   "configure": "add [server] url = \"https://<guard-serve-host>\" to target.toml, then run guard serve there"},
  {"id": "jev", "nodes": ["jev"], "label": "Jev triage", "state": "not_configured",
   "detail": "TYPESAFE_API_KEY is not set here and guard serve is not reachable",
   "configure": "set TYPESAFE_API_KEY where guard serve runs (GUARD_JEV_FAKE=1 for a scripted demo)"},
  {"id": "gatekeeper", "nodes": ["gate"], "label": "Gatekeeper", "state": "not_configured",
   "detail": "runs inside guard serve, which is not configured; Shadow Mode: every ticket goes to the human lane (defaults)",
   "configure": "the gatekeeper runs inside guard serve: add [server] url to target.toml; tune thresholds.toml (mode = \"active\")"},
  {"id": "agents", "nodes": ["qa", "seal", "coder"], "label": "Agent lane", "state": "working",
   "detail": "1 agent task(s) running: SHOP-142 qa on agent/SHOP-142",
   "tasks": [{"ticket": "SHOP-142", "role": "qa", "branch": "agent/SHOP-142", "attempts": 1}]},
  {"id": "gateway", "nodes": ["gateway"], "label": "Model gateway", "state": "connected",
   "detail": "LiteLLM at litellm.acme.internal:4000 is alive"},
  {"id": "skills", "nodes": ["skills"], "label": "Skills and AGENTS.md", "state": "configured",
   "detail": "agents.toml and AGENTS.md present (guard agents sync --check verifies them)"},
  {"id": "local", "nodes": ["local"], "label": "Local app", "state": "connected",
   "detail": "app ready at http://localhost:5173/"},
  {"id": "code_host", "nodes": ["direct", "devpr"], "label": "Code host", "state": "connected",
   "detail": "GitHub at api.github.com as acme-bot"},
  {"id": "ci", "nodes": ["guard"], "label": "Remote guard (CI)", "state": "not_configured",
   "detail": "no CI workflow runs guard gate yet",
   "configure": "add a workflow that runs guard gate plan/shard/finalize (see .github/workflows/guard.yml and ci/github-action, or the GitLab component ci/gitlab/templates/gate.yml)"}
]}"#;

fn commit(
    repo: Option<&str>,
    sha: &str,
    author: &str,
    ago: u64,
    subject: &str,
    files: &[&str],
) -> Suspect {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Suspect {
        repo: repo.map(str::to_string),
        sha: format!("{sha}{}", "0".repeat(40 - sha.len())),
        short: sha.into(),
        author: author.into(),
        date: String::new(),
        timestamp: now.saturating_sub(ago),
        subject: subject.into(),
        files: files.iter().map(|f| (*f).to_string()).collect(),
        recent: false,
    }
}

fn blame(file: &str, line: u32, by: Option<(&str, &str, u64, &str)>, content: &str) -> Blame {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Blame {
        file: file.into(),
        line,
        sha: by.map(|(sha, ..)| format!("{sha}{}", "0".repeat(40 - sha.len()))),
        short: by.map(|(sha, ..)| sha.to_string()),
        author: by.map(|(_, a, ..)| a.to_string()),
        timestamp: by.map(|(_, _, ago, _)| now.saturating_sub(ago)),
        subject: by.map(|(.., s)| s.to_string()),
        content: content.into(),
        uncommitted: by.is_none(),
    }
}

const HOUR: u64 = 3_600;
const DAY: u64 = 86_400;

fn demo_suspects() -> SuspectsDoc {
    let journey = |id: &str, commits, uncommitted: &[&str], blame| JourneySuspects {
        journey: Some(id.into()),
        title: String::new(),
        commits,
        uncommitted: uncommitted.iter().map(|f| (*f).to_string()).collect(),
        blame,
    };
    SuspectsDoc {
        journeys: vec![
            journey(
                "checkout.split-payment",
                vec![
                    commit(
                        Some("services"),
                        "4f9c2e1",
                        "Lucia Fernandez",
                        5 * HOUR,
                        "feat(checkout): apply gift card after the card charge",
                        &[
                            "services/checkout/payments/split.ts",
                            "services/checkout/payments/totals.ts",
                        ],
                    ),
                    commit(
                        None,
                        "b81d07a",
                        "Marco Ruiz",
                        2 * DAY,
                        "refactor(web): order-total component reads the cart store",
                        &["web/src/checkout/OrderTotal.tsx"],
                    ),
                ],
                &["web/src/checkout/GiftCard.tsx"],
                vec![blame(
                    "e2e-playwright/tests/journeys/checkout/split-payment.spec.ts",
                    42,
                    Some((
                        "1c0e5d3",
                        "Ana Torres",
                        9 * DAY,
                        "test(checkout): split payment journey",
                    )),
                    "await expect(page.getByTestId('order-total')).toHaveText('$ 84.90');",
                )],
            ),
            journey(
                "checkout.guest",
                vec![commit(
                    None,
                    "7a3be90",
                    "Marco Ruiz",
                    DAY,
                    "chore(web): move the dev server to port 5174",
                    &["web/src/checkout/vite.config.ts"],
                )],
                &[],
                vec![blame(
                    "e2e-playwright/tests/journeys/checkout/guest.spec.ts",
                    12,
                    None,
                    "await page.goto('/checkout');",
                )],
            ),
            journey("catalog.filters", Vec::new(), &[], Vec::new()),
        ],
        warnings: Vec::new(),
    }
}

/// What `GET /v1/models` could answer, for the settings shot.
const DEMO_MODELS: [(&str, &str); 4] = [
    ("claude-fable-5-1", "Claude Fable 5.1"),
    ("claude-opus-5-5", "Claude Opus 5.5"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    ("claude-haiku-5-5", "Claude Haiku 5.5"),
];

const DEMO_JOURNEY_SUMMARY: &str = "## checkout.split-payment
La aserción de `order-total` falla: la UI muestra \"$ 94.90\" pero el test espera \"$ 84.90\".

- El saldo de la gift card no se descuenta antes del cobro con tarjeta.
- Falla en `tests/journeys/checkout/split-payment.spec.ts:42`.

## Resumen
Un journey falló por un **total mal calculado**. Revisá cómo el flujo de pago dividido aplica la gift card antes de cobrar la tarjeta.";

const DEMO_ALL_SUMMARY: &str = "## checkout.split-payment
El total no descuenta la gift card: `order-total` muestra \"$ 94.90\" en vez de \"$ 84.90\".

## checkout.guest
`page.goto` devuelve `net::ERR_CONNECTION_REFUSED`: la app en `localhost:5173` no estaba levantada.

## catalog.filters
- Inestable: el checkbox `Brand` tarda más de 5000 ms en aparecer.

## Resumen
Dos fallas reales y una inestable. Empezá por **checkout.split-payment**; `checkout.guest` es un problema de entorno.";
