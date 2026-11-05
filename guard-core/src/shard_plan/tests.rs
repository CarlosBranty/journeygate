use super::*;

fn files(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn history(entries: &[(&str, u64)]) -> BTreeMap<String, u64> {
    entries.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

fn shard_files(shards: &[Shard]) -> Vec<Vec<&str>> {
    shards
        .iter()
        .map(|s| s.files.iter().map(String::as_str).collect())
        .collect()
}

#[test]
fn heaviest_file_goes_to_the_lightest_shard() {
    let plan = plan_shards(
        &files(&["a", "b", "c", "d"]),
        &history(&[("a", 10), ("b", 7), ("c", 5), ("d", 4)]),
        2,
    )
    .unwrap();
    assert_eq!(shard_files(&plan), vec![vec!["a", "d"], vec!["b", "c"]]);
    assert_eq!(plan[0].estimated_ms, 14);
    assert_eq!(plan[1].estimated_ms, 12);
}

#[test]
fn equal_durations_are_ordered_by_path_and_lightest_ties_go_to_the_lowest_index() {
    let plan = plan_shards(
        &files(&["c", "a", "b"]),
        &history(&[("a", 5), ("b", 5), ("c", 5)]),
        2,
    )
    .unwrap();
    assert_eq!(shard_files(&plan), vec![vec!["a", "c"], vec!["b"]]);
}

#[test]
fn plan_is_independent_of_input_order() {
    let h = history(&[("x", 3), ("y", 9), ("z", 3)]);
    let one = plan_shards(&files(&["x", "y", "z"]), &h, 2).unwrap();
    let two = plan_shards(&files(&["z", "y", "x"]), &h, 2).unwrap();
    assert_eq!(one, two);
}

#[test]
fn files_without_history_weigh_the_median_of_known_durations() {
    let weights = weigh(
        &files(&["a", "b", "c", "new"]),
        &history(&[("a", 100), ("b", 300), ("c", 200)]),
    );
    assert_eq!(
        weights,
        vec![
            ("b".into(), 300),
            ("c".into(), 200),
            ("new".into(), 200),
            ("a".into(), 100)
        ]
    );
}

#[test]
fn median_uses_every_history_entry_even_files_not_in_the_plan() {
    let weights = weigh(
        &files(&["new"]),
        &history(&[("gone1", 100), ("gone2", 400)]),
    );
    assert_eq!(weights, vec![("new".into(), 250)]);
}

#[test]
fn without_history_every_file_weighs_one_second() {
    let weights = weigh(&files(&["a", "b"]), &BTreeMap::new());
    assert_eq!(weights, vec![("a".into(), 1000), ("b".into(), 1000)]);
}

#[test]
fn median_rounds_even_counts_half_up() {
    assert_eq!(median(&[]), None);
    assert_eq!(median(&[7]), Some(7));
    assert_eq!(median(&[3, 1, 2]), Some(2));
    assert_eq!(median(&[1, 2]), Some(2));
    assert_eq!(median(&[10, 20, 30, 40]), Some(25));
}

#[test]
fn extra_shards_are_empty() {
    let plan = plan_shards(&files(&["a"]), &BTreeMap::new(), 3).unwrap();
    assert_eq!(shard_files(&plan), vec![vec!["a"], vec![], vec![]]);
    assert_eq!(plan[2].estimated_ms, 0);
}

#[test]
fn zero_shards_is_an_error() {
    assert_eq!(
        plan_shards(&files(&["a"]), &BTreeMap::new(), 0),
        Err(ShardPlanError::ZeroShards)
    );
}

#[test]
fn plan_entries_are_prefixed_for_absolute_test_match() {
    let shard = Shard {
        files: files(&["tests/smoke/healthz.spec.ts"]),
        estimated_ms: 1,
    };
    assert_eq!(shard.plan_entries(), vec!["**/tests/smoke/healthz.spec.ts"]);
}

#[test]
fn shard_plan_file_has_exactly_one_files_key() {
    let shard = Shard {
        files: files(&["tests/a.spec.ts"]),
        estimated_ms: 1,
    };
    let json: serde_json::Value = serde_json::from_str(&shard.plan_json()).unwrap();
    assert_eq!(json, serde_json::json!({"files": ["**/tests/a.spec.ts"]}));
}

/// No spec files: every shard is empty, so every `gate shard` prints
/// "nothing to run" and `gate finalize` then fails with exit 3 (no blobs).
#[test]
fn an_empty_file_list_yields_empty_shards() {
    let plan = plan_shards(&[], &history(&[("a", 10)]), 3).unwrap();
    assert_eq!(plan.len(), 3);
    assert!(
        plan.iter()
            .all(|s| s.files.is_empty() && s.estimated_ms == 0)
    );
    assert_eq!(plan[0].plan_json(), "{\n  \"files\": []\n}");
}

#[test]
fn duplicate_input_files_are_planned_once() {
    let plan = plan_shards(
        &files(&["b", "a", "b", "a"]),
        &history(&[("a", 5), ("b", 5)]),
        2,
    )
    .unwrap();
    assert_eq!(shard_files(&plan), vec![vec!["a"], vec!["b"]]);
    assert_eq!(
        weigh(&files(&["x", "x", "y"]), &BTreeMap::new()),
        vec![
            ("x".to_string(), DEFAULT_WEIGHT_MS),
            ("y".to_string(), DEFAULT_WEIGHT_MS)
        ]
    );
}
