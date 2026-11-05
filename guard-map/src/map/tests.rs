use super::*;
use crate::fake::FakeGraphSource;
use crate::{MAX_TOKEN_BUDGET, MIN_TOKEN_BUDGET};

fn req(focus: &[&str], mentioned: &[&str], budget: u32) -> MapRequest {
    MapRequest {
        focus_files: focus.iter().map(|s| s.to_string()).collect(),
        mentioned: mentioned.iter().map(|s| s.to_string()).collect(),
        token_budget: budget,
    }
}

fn sample() -> FakeGraphSource {
    FakeGraphSource::new("fake", "abc123")
        .edge("src/app.ts", "src/router.ts")
        .edge("src/app.ts", "src/context.ts")
        .edge("src/router.ts", "src/utils.ts")
        .edge("src/other.ts", "src/misc.ts")
        .symbol("src/router.ts", "Router", "class", 3)
        .symbol("src/router.ts", "match", "function", 20)
        .symbol("src/misc.ts", "parseTicket", "function", 7)
}

/// Deterministic xorshift64* so the property test needs no extra crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn random_source(rng: &mut Rng) -> FakeGraphSource {
    let n = 1 + rng.below(300) as usize;
    let names: Vec<String> = (0..n)
        .map(|i| {
            let depth = rng.below(4);
            let dirs: String = (0..depth)
                .map(|d| format!("dir{d}_{}/", rng.below(9)))
                .collect();
            format!("{dirs}file_{i}_{}.ts", "x".repeat(rng.below(40) as usize))
        })
        .collect();
    let mut s = FakeGraphSource::new("fake", &"c".repeat(1 + rng.below(64) as usize));
    for f in &names {
        s = s.file(f);
        for k in 0..rng.below(12) {
            let name = format!("sym_{k}_{}", "y".repeat(rng.below(30) as usize));
            s = s.symbol(f, &name, "function", 1 + rng.below(5000) as u32);
        }
    }
    for _ in 0..rng.below(4 * n as u64) {
        let a = &names[rng.below(n as u64) as usize];
        let b = &names[rng.below(n as u64) as usize];
        s = s.edge(a, b);
    }
    s
}

#[test]
fn focus_file_ranks_first_and_dependencies_follow() {
    let r = build_map(&sample(), &req(&["src/app.ts"], &[], 1024)).unwrap();
    assert_eq!(r.backend, "fake");
    assert_eq!(r.commit, "abc123");
    let order: Vec<&str> = r.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(order[0], "src/app.ts", "{order:?}");
    let pos = |p: &str| order.iter().position(|x| *x == p).unwrap();
    assert!(pos("src/router.ts") < pos("src/misc.ts"));
    assert!(pos("src/context.ts") < pos("src/other.ts"));
    let router = &r.entries[pos("src/router.ts")];
    assert_eq!(router.symbols, ["Router:3", "match:20"]);
    for w in r.entries.windows(2) {
        assert!(w[0].rank >= w[1].rank);
    }
    assert_eq!(r.omitted, 0);
}

#[test]
fn mentioned_identifier_seeds_its_defining_file() {
    let none = build_map(&sample(), &req(&[], &[], 1024)).unwrap();
    let with = build_map(&sample(), &req(&[], &["parseTicket"], 1024)).unwrap();
    let rank_of = |r: &MapResponse| {
        r.entries
            .iter()
            .find(|e| e.path == "src/misc.ts")
            .unwrap()
            .rank
    };
    assert!(rank_of(&with) > rank_of(&none));
    assert_eq!(with.entries[0].path, "src/misc.ts");
}

#[test]
fn unknown_focus_files_are_ignored_and_paths_are_normalized() {
    let a = build_map(&sample(), &req(&["./src\\app.ts", "nope.ts"], &[], 1024)).unwrap();
    let b = build_map(&sample(), &req(&["src/app.ts"], &[], 1024)).unwrap();
    assert_eq!(a, b);
}

#[test]
fn budget_is_validated() {
    for bad in [0, MIN_TOKEN_BUDGET - 1, MAX_TOKEN_BUDGET + 1] {
        assert!(matches!(
            build_map(&sample(), &req(&[], &[], bad)),
            Err(MapError::InvalidRequest(_))
        ));
    }
}

#[test]
fn unavailable_source_is_reported() {
    let mut s = sample();
    s.unavailable = Some("index missing".into());
    assert_eq!(
        build_map(&s, &req(&[], &[], 1024)),
        Err(MapError::Unavailable("index missing".into()))
    );
}

#[test]
fn envelope_too_large_for_budget_is_an_error_not_an_overrun() {
    let s = FakeGraphSource::new("fake", &"c".repeat(600)).file("a.ts");
    assert!(matches!(
        build_map(&s, &req(&[], &[], MIN_TOKEN_BUDGET)),
        Err(MapError::InvalidRequest(_))
    ));
}

#[test]
fn trimming_drops_symbols_then_entries_and_counts_omitted() {
    let mut s = FakeGraphSource::new("fake", "abc");
    for i in 0..200 {
        let f = format!("src/module_{i:03}.ts");
        s = s.symbol(&f, &format!("someLongExportedName{i}"), "function", 1);
    }
    let r = build_map(&s, &req(&[], &[], MIN_TOKEN_BUDGET)).unwrap();
    assert!(!r.entries.is_empty());
    assert!(r.omitted > 0);
    assert_eq!(r.entries.len() as u32 + r.omitted, 200);
    let wire = serde_json::to_string(&r).unwrap();
    assert!(estimate_tokens(&wire) <= MIN_TOKEN_BUDGET);
    assert_eq!(r.estimated_tokens, estimate_tokens(&wire));
}

#[test]
fn budget_is_never_exceeded_over_random_graphs() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for case in 0..250 {
        let s = random_source(&mut rng);
        let budget =
            MIN_TOKEN_BUDGET + rng.below(u64::from(MAX_TOKEN_BUDGET - MIN_TOKEN_BUDGET) + 1) as u32;
        let focus: Vec<String> = (0..rng.below(4))
            .map(|_| s.files[rng.below(s.files.len() as u64) as usize].clone())
            .collect();
        let request = MapRequest {
            focus_files: focus,
            mentioned: vec!["sym_1_".into()],
            token_budget: budget,
        };
        let r = match build_map(&s, &request) {
            Ok(r) => r,
            Err(e) => panic!("case {case}: {e}"),
        };
        let wire = serde_json::to_string(&r).unwrap();
        let actual = estimate_tokens(&wire);
        assert!(actual <= budget, "case {case}: {actual} > {budget}");
        assert!(r.estimated_tokens >= actual && r.estimated_tokens <= budget);
        assert_eq!(r.entries.len() as u32 + r.omitted, s.files.len() as u32);
        assert!(
            r.entries
                .iter()
                .all(|e| e.symbols.len() <= MAX_SYMBOLS_PER_FILE)
        );
    }
}

#[test]
fn ticket_paths_are_extracted_and_merged_after_changed_files() {
    let files: Vec<String> = ["src/a.ts", "src/b.ts", "docs/c.md"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let text = "Crash in `src/b.ts:42`, see (docs/c.md). Also src/a.ts. Not src/zzz.ts";
    assert_eq!(
        mentioned_paths(text, &files),
        ["src/b.ts", "docs/c.md", "src/a.ts"]
    );
    assert_eq!(
        focus_files(&["src/a.ts".into()], text, &files),
        ["src/a.ts", "src/b.ts", "docs/c.md"]
    );
}
