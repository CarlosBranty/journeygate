use super::*;

fn pattern(p: &str) -> Regex {
    compile_pattern(p).unwrap()
}

#[test]
fn extracts_the_key_from_a_branch_name() {
    let re = pattern(r"SHOP-\d+");
    assert_eq!(
        extract_ticket(&re, Some("SHOP-123-fix-cart")),
        Some("SHOP-123".into())
    );
    assert_eq!(
        extract_ticket(&re, Some("feature/SHOP-7-x")),
        Some("SHOP-7".into())
    );
}

#[test]
fn takes_the_first_match() {
    let re = pattern(r"[A-Z]+-\d+");
    assert_eq!(
        extract_ticket(&re, Some("ABC-1-and-DEF-2")),
        Some("ABC-1".into())
    );
}

#[test]
fn detached_head_and_missing_branches_have_no_ticket() {
    let re = pattern(r"SHOP-\d+");
    assert_eq!(extract_ticket(&re, None), None);
    assert_eq!(extract_ticket(&re, Some("HEAD")), None);
    assert_eq!(extract_ticket(&re, Some("main")), None);
}

#[test]
fn invalid_patterns_fail_to_compile() {
    assert!(compile_pattern("([").is_err());
}

#[test]
fn builds_the_ticket_tag() {
    assert_eq!(ticket_tag("SHOP-123"), "@ticket:SHOP-123");
}
