//! [`summarize_diff`]: the deterministic, size-bounded view of a `git diff`
//! that the impact questions see (limits in `docs/contracts/jev-questions.md`).

use guard_core::ports::{DiffFile, DiffHunk, DiffSummary, FileStatus};

pub const MAX_FILES: usize = 200;
pub const MAX_SYMBOLS: usize = 300;
pub const MAX_HUNKS: usize = 40;
pub const MAX_HUNK_LINES: usize = 12;
pub const MAX_LINE_CHARS: usize = 200;
pub const MAX_SUMMARY_JSON_BYTES: usize = 48_000;

#[derive(Default)]
struct FileAcc {
    old_path: Option<String>,
    new_path: Option<String>,
    status: Option<FileStatus>,
    additions: u32,
    deletions: u32,
}

impl FileAcc {
    fn path(&self) -> Option<&str> {
        match self.status {
            Some(FileStatus::Deleted) => self.old_path.as_deref().or(self.new_path.as_deref()),
            _ => self.new_path.as_deref().or(self.old_path.as_deref()),
        }
    }

    fn finish(self) -> Option<DiffFile> {
        let path = self.path()?.to_string();
        Some(DiffFile {
            path,
            status: self.status.unwrap_or(FileStatus::Modified),
            additions: self.additions,
            deletions: self.deletions,
        })
    }
}

/// Summarizes a unified diff (`git diff --no-renames` output; renames and plain
/// `---`/`+++` diffs are accepted too). Never fails: unknown lines are skipped.
pub fn summarize_diff(unified_diff: &str) -> DiffSummary {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut symbols: Vec<String> = Vec::new();
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut truncated = false;

    let mut current: Option<FileAcc> = None;
    let lines: Vec<&str> = unified_diff.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(f) = current.take().and_then(FileAcc::finish) {
                files.push(f);
            }
            let (a, b) = split_git_paths(rest);
            current = Some(FileAcc {
                old_path: a,
                new_path: b,
                ..FileAcc::default()
            });
        } else if line.starts_with("--- ")
            && lines.get(i + 1).is_some_and(|n| n.starts_with("+++ "))
        {
            let old = header_path(&line[4..]);
            let new = header_path(&lines[i + 1][4..]);
            // A plain unified diff has no `diff --git` line: each pair starts a file.
            let starts_new = current
                .as_ref()
                .is_none_or(|c| c.additions + c.deletions > 0);
            if starts_new {
                if let Some(f) = current.take().and_then(FileAcc::finish) {
                    files.push(f);
                }
                current = Some(FileAcc::default());
            }
            let acc = current.get_or_insert_with(FileAcc::default);
            match (&old, &new) {
                (None, Some(_)) => acc.status = Some(FileStatus::Added),
                (Some(_), None) => acc.status = Some(FileStatus::Deleted),
                _ => {}
            }
            if old.is_some() {
                acc.old_path = old;
            }
            if new.is_some() {
                acc.new_path = new;
            }
            i += 2;
            continue;
        } else if let Some(acc) = current.as_mut() {
            if line.starts_with("new file mode") {
                acc.status = Some(FileStatus::Added);
            } else if line.starts_with("deleted file mode") {
                acc.status = Some(FileStatus::Deleted);
            } else if let Some(p) = line.strip_prefix("rename from ") {
                acc.status = Some(FileStatus::Renamed);
                acc.old_path = Some(p.to_string());
            } else if let Some(p) = line.strip_prefix("rename to ") {
                acc.status = Some(FileStatus::Renamed);
                acc.new_path = Some(p.to_string());
            } else if line.starts_with("@@") {
                let Some((mut old_left, mut new_left)) = hunk_counts(line) else {
                    i += 1;
                    continue;
                };
                let path = acc.path().unwrap_or_default().to_string();
                if let Some(sym) = header_context(line).and_then(declared_symbol) {
                    symbols.push(format!("{path}#{sym}"));
                }
                let mut body: Vec<String> = Vec::new();
                i += 1;
                while i < lines.len()
                    && (old_left > 0 || new_left > 0 || lines[i].starts_with('\\'))
                {
                    let l = lines[i];
                    match l.chars().next() {
                        Some('+') => {
                            acc.additions += 1;
                            new_left = new_left.saturating_sub(1);
                            if let Some(sym) = declared_symbol(&l[1..]) {
                                symbols.push(format!("{path}#{sym}"));
                            }
                        }
                        Some('-') => {
                            acc.deletions += 1;
                            old_left = old_left.saturating_sub(1);
                            if let Some(sym) = declared_symbol(&l[1..]) {
                                symbols.push(format!("{path}#{sym}"));
                            }
                        }
                        Some('\\') => {}
                        _ => {
                            old_left = old_left.saturating_sub(1);
                            new_left = new_left.saturating_sub(1);
                        }
                    }
                    if body.len() < MAX_HUNK_LINES {
                        body.push(cut(l, &mut truncated));
                    } else {
                        truncated = true;
                    }
                    i += 1;
                }
                if hunks.len() < MAX_HUNKS {
                    hunks.push(DiffHunk {
                        path,
                        header: cut(line, &mut truncated),
                        lines: body,
                    });
                } else {
                    truncated = true;
                }
                continue;
            }
        }
        i += 1;
    }
    if let Some(f) = current.take().and_then(FileAcc::finish) {
        files.push(f);
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    if files.len() > MAX_FILES {
        files.truncate(MAX_FILES);
        truncated = true;
    }
    symbols.sort();
    symbols.dedup();
    if symbols.len() > MAX_SYMBOLS {
        symbols.truncate(MAX_SYMBOLS);
        truncated = true;
    }

    let mut summary = DiffSummary {
        files,
        symbols,
        hunks,
        truncated,
    };
    fit_size(&mut summary);
    summary
}

fn json_len(s: &DiffSummary) -> usize {
    serde_json::to_vec(s).map_or(usize::MAX, |v| v.len())
}

/// Drops hunks from the end, then symbols, then files, until the JSON fits.
fn fit_size(s: &mut DiffSummary) {
    if json_len(s) <= MAX_SUMMARY_JSON_BYTES {
        return;
    }
    s.truncated = true;
    while json_len(s) > MAX_SUMMARY_JSON_BYTES && s.hunks.pop().is_some() {}
    while json_len(s) > MAX_SUMMARY_JSON_BYTES && s.symbols.pop().is_some() {}
    while json_len(s) > MAX_SUMMARY_JSON_BYTES && s.files.pop().is_some() {}
}

fn cut(line: &str, truncated: &mut bool) -> String {
    if line.chars().count() > MAX_LINE_CHARS {
        *truncated = true;
        line.chars().take(MAX_LINE_CHARS).collect()
    } else {
        line.to_string()
    }
}

/// `a/x b/y` -> (`x`, `y`). Paths with spaces are split at ` b/`.
fn split_git_paths(rest: &str) -> (Option<String>, Option<String>) {
    match rest.split_once(" b/") {
        Some((a, b)) => (
            Some(a.strip_prefix("a/").unwrap_or(a).to_string()),
            Some(b.to_string()),
        ),
        None => (None, None),
    }
}

/// `a/x` -> `x`; `/dev/null` -> `None`. A trailing tab + timestamp is dropped.
fn header_path(raw: &str) -> Option<String> {
    let raw = raw.split('\t').next().unwrap_or(raw).trim_end();
    if raw == "/dev/null" {
        return None;
    }
    let p = raw
        .strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw);
    Some(p.to_string())
}

/// `@@ -1,3 +1,4 @@ ...` -> (3, 4). A missing count means 1.
fn hunk_counts(header: &str) -> Option<(u32, u32)> {
    let inner = header.strip_prefix("@@ ")?;
    let (ranges, _) = inner.split_once(" @@")?;
    let mut parts = ranges.split_whitespace();
    let count = |r: &str| -> Option<u32> {
        match r.split_once(',') {
            Some((_, n)) => n.parse().ok(),
            None => Some(1),
        }
    };
    let old = count(parts.next()?.strip_prefix('-')?)?;
    let new = count(parts.next()?.strip_prefix('+')?)?;
    Some((old, new))
}

/// The function context git appends after the second `@@`.
fn header_context(header: &str) -> Option<&str> {
    let inner = header.strip_prefix("@@ ")?;
    let (_, ctx) = inner.split_once(" @@")?;
    let ctx = ctx.strip_prefix(' ').unwrap_or(ctx);
    (!ctx.is_empty()).then_some(ctx)
}

/// The name declared by a top-level (unindented) declaration line, if any:
/// functions, classes, types and exported bindings in TS/JS, Python, Go and Rust.
pub(crate) fn declared_symbol(line: &str) -> Option<String> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    const MODIFIERS: [&str; 14] = [
        "export",
        "default",
        "async",
        "pub",
        "pub(crate)",
        "pub(super)",
        "public",
        "static",
        "abstract",
        "declare",
        "unsafe",
        "extern",
        "final",
        "const",
    ];
    const DECLS: [&str; 10] = [
        "function",
        "function*",
        "class",
        "def",
        "func",
        "fn",
        "struct",
        "interface",
        "enum",
        "trait",
    ];
    const BINDINGS: [&str; 5] = ["type", "let", "var", "mod", "const"];
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut exported = false;
    let mut idx = 0;
    while idx < tokens.len() {
        let t = tokens[idx];
        if DECLS.contains(&t) {
            let mut rest = tokens[idx + 1..].join(" ");
            if t == "func" && rest.starts_with('(') {
                // Go method: skip the receiver.
                let close = rest.find(')')?;
                rest = rest[close + 1..].trim_start().to_string();
            }
            return ident(&rest);
        }
        // Bindings (`const`, `let`, `var`) only count when exported; a bare
        // `const` falls through as a modifier (`const fn`).
        if BINDINGS.contains(&t) && (exported || t == "type" || t == "mod") {
            return tokens.get(idx + 1).and_then(|n| ident(n));
        }
        if MODIFIERS.contains(&t) {
            exported |= t == "export" || t.starts_with("pub");
            idx += 1;
            continue;
        }
        return None;
    }
    None
}

fn ident(s: &str) -> Option<String> {
    let s = s.trim_start_matches('*').trim_start();
    let name: String = s
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    (!name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit())).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/web/src/cart.ts b/web/src/cart.ts
index 1111111..2222222 100644
--- a/web/src/cart.ts
+++ b/web/src/cart.ts
@@ -1,4 +1,5 @@ export class Cart {
 import { fmt } from './money';
-export function total(c: number) {
+export function total(c: number, tax: number) {
+  const x = 1;
   return fmt(c);
 }
diff --git a/services/orders/new.go b/services/orders/new.go
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/services/orders/new.go
@@ -0,0 +1,3 @@
+package orders
+
+func (s *Service) Place(id string) error { return nil }
diff --git a/old.py b/old.py
deleted file mode 100644
index 4444444..0000000
--- a/old.py
+++ /dev/null
@@ -1,2 +0,0 @@
-def legacy():
-    pass
diff --git a/img.png b/img.png
index 5555555..6666666 100644
Binary files a/img.png and b/img.png differ
";

    #[test]
    fn files_statuses_and_counts() {
        let s = summarize_diff(DIFF);
        let got: Vec<(&str, FileStatus, u32, u32)> = s
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.additions, f.deletions))
            .collect();
        assert_eq!(
            got,
            vec![
                ("img.png", FileStatus::Modified, 0, 0),
                ("old.py", FileStatus::Deleted, 0, 2),
                ("services/orders/new.go", FileStatus::Added, 3, 0),
                ("web/src/cart.ts", FileStatus::Modified, 2, 1),
            ]
        );
        assert!(!s.truncated);
    }

    #[test]
    fn symbols_come_from_declarations_and_hunk_context() {
        let s = summarize_diff(DIFF);
        assert_eq!(
            s.symbols,
            vec![
                "old.py#legacy",
                "services/orders/new.go#Place",
                "web/src/cart.ts#Cart",
                "web/src/cart.ts#total",
            ]
        );
    }

    #[test]
    fn hunks_keep_header_and_body() {
        let s = summarize_diff(DIFF);
        assert_eq!(s.hunks.len(), 3);
        assert_eq!(s.hunks[0].path, "web/src/cart.ts");
        assert_eq!(s.hunks[0].header, "@@ -1,4 +1,5 @@ export class Cart {");
        assert_eq!(s.hunks[0].lines.len(), 6);
        assert_eq!(s.hunks[0].lines[1], "-export function total(c: number) {");
        assert_eq!(s.hunks[2].path, "old.py");
    }

    #[test]
    fn summarize_diff_tolerates_submodule_header_lines() {
        let diff = "Submodule frontend 1111111..2222222:
diff --git a/frontend/src/login.tsx b/frontend/src/login.tsx
index 3333333..4444444 100644
--- a/frontend/src/login.tsx
+++ b/frontend/src/login.tsx
@@ -1 +1 @@
-old
+new
Submodule backend contains untracked content
Submodule backend contains modified content
Submodule docs 5555555...6666666 (commits not present)
";
        let s = summarize_diff(diff);
        let got: Vec<(&str, u32, u32)> = s
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.additions, f.deletions))
            .collect();
        assert_eq!(got, vec![("frontend/src/login.tsx", 1, 1)]);
        assert_eq!(s.hunks.len(), 1);
        assert_eq!(s.hunks[0].lines, vec!["-old", "+new"]);
    }

    #[test]
    fn a_deleted_line_that_looks_like_a_header_stays_in_the_hunk() {
        let diff = "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n@@ -1,2 +1,1 @@\n--- not a header\n keep\n";
        let s = summarize_diff(diff);
        assert_eq!(s.files.len(), 1);
        assert_eq!(s.files[0].deletions, 1);
        assert_eq!(s.hunks[0].lines, vec!["--- not a header", " keep"]);
    }

    #[test]
    fn plain_unified_diffs_and_renames() {
        let plain = "--- a/x.ts\t2024-01-01\n+++ b/x.ts\t2024-01-02\n@@ -1 +1 @@\n-a\n+b\n--- a/y.ts\n+++ b/y.ts\n@@ -1 +1 @@\n-c\n+d\n";
        let s = summarize_diff(plain);
        let paths: Vec<&str> = s.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["x.ts", "y.ts"]);
        let renamed = "diff --git a/old.ts b/new.ts\nsimilarity index 90%\nrename from old.ts\nrename to new.ts\n";
        let s = summarize_diff(renamed);
        assert_eq!(s.files[0].path, "new.ts");
        assert_eq!(s.files[0].status, FileStatus::Renamed);
    }

    #[test]
    fn hunk_lines_and_long_lines_are_cut() {
        let long = "x".repeat(300);
        let mut diff =
            String::from("diff --git a/a.ts b/a.ts\n--- a/a.ts\n+++ b/a.ts\n@@ -0,0 +1,20 @@\n");
        for _ in 0..20 {
            diff.push_str(&format!("+{long}\n"));
        }
        let s = summarize_diff(&diff);
        assert_eq!(s.files[0].additions, 20);
        assert_eq!(s.hunks[0].lines.len(), MAX_HUNK_LINES);
        assert_eq!(s.hunks[0].lines[0].chars().count(), MAX_LINE_CHARS);
        assert!(s.truncated);
    }

    #[test]
    fn file_hunk_and_size_limits_hold() {
        let mut diff = String::new();
        for n in 0..250 {
            diff.push_str(&format!(
                "diff --git a/f{n:03}.ts b/f{n:03}.ts\n--- a/f{n:03}.ts\n+++ b/f{n:03}.ts\n@@ -1 +1 @@\n-export function old{n}() {{}}\n+export function new{n}() {{ {} }}\n",
                "y".repeat(150)
            ));
        }
        let s = summarize_diff(&diff);
        assert!(s.truncated);
        assert!(s.files.len() <= MAX_FILES);
        assert!(s.symbols.len() <= MAX_SYMBOLS);
        assert!(s.hunks.len() <= MAX_HUNKS);
        assert!(serde_json::to_vec(&s).unwrap().len() <= MAX_SUMMARY_JSON_BYTES);
        assert_eq!(s.files[0].path, "f000.ts");
    }

    #[test]
    fn oversized_json_drops_hunks_first() {
        let mut s = DiffSummary {
            files: vec![DiffFile {
                path: "a".into(),
                status: FileStatus::Modified,
                additions: 1,
                deletions: 0,
            }],
            symbols: vec!["a#b".into()],
            hunks: (0..40)
                .map(|_| DiffHunk {
                    path: "a".into(),
                    header: "@@".into(),
                    lines: vec!["z".repeat(1990)],
                })
                .collect(),
            truncated: false,
        };
        fit_size(&mut s);
        assert!(s.truncated);
        assert!(s.hunks.len() < 40);
        assert_eq!(s.symbols.len(), 1);
        assert_eq!(s.files.len(), 1);
    }

    #[test]
    fn empty_input_is_an_empty_summary() {
        assert_eq!(summarize_diff(""), DiffSummary::default());
    }

    #[test]
    fn declarations_per_language() {
        let cases = [
            (
                "export default async function handler(req) {",
                Some("handler"),
            ),
            ("export const PriceTag = () => null;", Some("PriceTag")),
            ("const local = 1;", None),
            ("class Cart extends Base {", Some("Cart")),
            ("def place_order(x):", Some("place_order")),
            ("    def method(self):", None),
            ("func Format(c int) string {", Some("Format")),
            ("pub(crate) fn build() -> Self {", Some("build")),
            ("pub struct Order {", Some("Order")),
            ("type Price = number;", Some("Price")),
            ("pub const MAX: usize = 1;", Some("MAX")),
            ("import x from './x';", None),
            ("return 1;", None),
        ];
        for (line, want) in cases {
            assert_eq!(declared_symbol(line).as_deref(), want, "{line}");
        }
    }
}
