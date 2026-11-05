use guard_core::config::parse_target;
use impact_vs_codegraph::{bounded_dependents, config_for};

#[test]
fn bounded_dependents_stop_at_depth_and_exclude_changed_files() {
    // chain: d -> c -> b -> a (d imports c, ...); e imports a.
    let sources = [
        ("a.ts", "export const a = 1;"),
        ("b.ts", "import { a } from './a';"),
        ("c.ts", "import './b';"),
        ("d.ts", "import './c';"),
        ("z.ts", "import './d';"),
        ("e.ts", "import { a } from './a';"),
    ];
    let config = parse_target(&config_for("typescript")).expect("config");
    let index = guard_impact::ImpactIndex::from_sources(sources, &config).expect("index");
    let changed = vec!["a.ts".to_string()];
    assert_eq!(
        bounded_dependents(&index, &changed, 3),
        vec!["b.ts", "c.ts", "d.ts", "e.ts"]
    );
    assert_eq!(
        bounded_dependents(&index, &changed, 1),
        vec!["b.ts", "e.ts"]
    );
    let two = vec!["a.ts".to_string(), "b.ts".to_string()];
    assert_eq!(bounded_dependents(&index, &two, 1), vec!["c.ts", "e.ts"]);
}

#[test]
fn config_for_produces_a_valid_target() {
    for lang in ["typescript", "python", "go", "rust", "csharp", "php"] {
        parse_target(&config_for(lang)).expect(lang);
    }
}
