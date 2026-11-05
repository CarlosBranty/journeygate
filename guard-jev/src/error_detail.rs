//! Human-readable detail of a rejected (`400`, `422`, other non-200) response.

/// Most characters kept from an error body.
pub const MAX_DETAIL_CHARS: usize = 2_000;

/// Extracts the useful part of an error body.
///
/// - `{"detail": "text"}` -> `text`
/// - `{"detail": [{"loc": [..], "msg": ".."}, ..]}` (validation errors) ->
///   `loc.joined.by.dots: msg; ...`
/// - `{"error": {"message": ".."}}`, `{"error": ".."}`, `{"message": ".."}` -> that text
/// - anything else -> the body as-is (trimmed)
///
/// The result is cut to [`MAX_DETAIL_CHARS`].
pub fn error_detail(body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| from_json(&v));
    truncate(parsed.unwrap_or_else(|| body.trim().to_string()))
}

fn from_json(v: &serde_json::Value) -> Option<String> {
    if let Some(detail) = v.get("detail") {
        return match detail {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Array(items) if !items.is_empty() => Some(
                items
                    .iter()
                    .map(validation_item)
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            serde_json::Value::Null => None,
            other => Some(other.to_string()),
        };
    }
    match v.get("error") {
        Some(serde_json::Value::String(s)) => return Some(s.clone()),
        Some(e) => {
            if let Some(m) = e.get("message").and_then(|m| m.as_str()) {
                return Some(m.to_string());
            }
        }
        None => {}
    }
    v.get("message")
        .and_then(|m| m.as_str())
        .map(str::to_string)
}

fn validation_item(item: &serde_json::Value) -> String {
    let msg = item
        .get("msg")
        .or_else(|| item.get("message"))
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| item.to_string());
    let loc = item.get("loc").and_then(|l| l.as_array()).map(|parts| {
        parts
            .iter()
            .map(|p| match p {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(".")
    });
    match loc {
        Some(loc) if !loc.is_empty() => format!("{loc}: {msg}"),
        _ => msg,
    }
}

fn truncate(s: String) -> String {
    if s.chars().count() <= MAX_DETAIL_CHARS {
        s
    } else {
        let mut cut: String = s.chars().take(MAX_DETAIL_CHARS).collect();
        cut.push_str("...");
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_detail() {
        assert_eq!(error_detail(r#"{"detail": "bad model"}"#), "bad model");
    }

    #[test]
    fn validation_detail_list() {
        let body = r#"{"detail": [{"loc": ["body", "questions", 0], "msg": "too long"}, {"msg": "no loc"}]}"#;
        assert_eq!(error_detail(body), "body.questions.0: too long; no loc");
    }

    #[test]
    fn error_object_and_message_shapes() {
        assert_eq!(
            error_detail(r#"{"error": {"type": "x", "message": "overloaded"}}"#),
            "overloaded"
        );
        assert_eq!(error_detail(r#"{"error": "nope"}"#), "nope");
        assert_eq!(error_detail(r#"{"message": "hi"}"#), "hi");
    }

    #[test]
    fn non_json_and_unknown_json_fall_back_to_body() {
        assert_eq!(error_detail("  plain text \n"), "plain text");
        assert_eq!(error_detail(r#"{"code": 7}"#), r#"{"code": 7}"#);
        assert_eq!(error_detail(""), "");
    }

    #[test]
    fn long_bodies_are_truncated() {
        let body = "x".repeat(MAX_DETAIL_CHARS + 50);
        let out = error_detail(&body);
        assert_eq!(out.chars().count(), MAX_DETAIL_CHARS + 3);
        assert!(out.ends_with("..."));
    }
}
