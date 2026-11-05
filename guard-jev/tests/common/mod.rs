#![allow(dead_code)]

use guard_core::ports::{
    Area, AreaOption, BlastRadius, DiffFile, DiffSummary, FileStatus, JourneyQuestion,
    TriageRequest, TriageState,
};

pub fn fixture(name: &str) -> String {
    let path = format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

pub fn fixture_json(name: &str) -> serde_json::Value {
    serde_json::from_str(&fixture(name)).expect("fixture is valid JSON")
}

pub fn areas() -> Vec<AreaOption> {
    vec![
        AreaOption {
            key: Area::new("account"),
            description: "Sign-up, login, profile and order history.".into(),
        },
        AreaOption {
            key: Area::new("catalog"),
            description: "Product listing, search, product detail pages and stock display.".into(),
        },
        AreaOption {
            key: Area::new("checkout"),
            description: "Cart, payment form, order placement and confirmation.".into(),
        },
    ]
}

pub fn triage_state() -> TriageState {
    TriageState {
        summary: "Stock badge shows 'In stock' for sold-out products".into(),
        description: "On the product page the badge ignores reservations and shows 'In stock' when every unit is reserved.".into(),
        acceptance_criteria: vec!["A product with 0 available units shows 'Sold out'".into()],
        mentioned_modules: vec!["examples/demo-shop/src/products/stock.mjs".into()],
        blast_radius: BlastRadius::Isolated,
    }
}

pub fn triage_request(seed: Option<u64>) -> TriageRequest {
    TriageRequest {
        ticket_key: "SHOP-42".into(),
        state: triage_state(),
        areas: areas(),
        permutation_seed: seed,
    }
}

pub fn journeys(n: usize) -> Vec<JourneyQuestion> {
    (0..n)
        .map(|i| JourneyQuestion {
            id: format!("shop.journey-{i:02}"),
            impact_question: format!("The diff affects journey number {i}."),
        })
        .collect()
}

pub fn diff() -> DiffSummary {
    DiffSummary {
        files: vec![DiffFile {
            path: "examples/demo-shop/src/products/stock.mjs".into(),
            status: FileStatus::Modified,
            additions: 3,
            deletions: 1,
        }],
        symbols: vec!["examples/demo-shop/src/products/stock.mjs#availableUnits".into()],
        hunks: vec![],
        truncated: false,
    }
}
