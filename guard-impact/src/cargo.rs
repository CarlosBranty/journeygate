//! Cargo manifests: the packages of a repository and the root file of every
//! target, the way Cargo discovers them (`[lib]`, `[[bin]]`, `[[test]]`,
//! `[[example]]`, `[[bench]]` paths, the `src/` conventions and `build.rs`),
//! plus the path dependencies each package can name in `use` paths.

use crate::resolve::{join_dir, normalize, parent};
use std::collections::{BTreeMap, BTreeSet};
use toml::{Table, Value};

/// One Cargo package of the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustCrate {
    /// Directory holding `Cargo.toml` (`""` at the repo root).
    pub(crate) dir: String,
    /// Name `use` paths see for the lib target (`[lib] name`, else the package
    /// name), with `-` replaced by `_`.
    pub(crate) lib_name: String,
    /// Root file of the lib target, when the package has one.
    pub(crate) lib_root: Option<String>,
    /// Root files of the other targets (bins, tests, examples, benches,
    /// build script) that exist in the repository.
    pub(crate) other_roots: Vec<String>,
    /// Extern names of path dependencies -> the dependency's package directory.
    pub(crate) path_deps: BTreeMap<String, String>,
}

/// Packages of every `Cargo.toml` with a `[package]` table. `read` returns a
/// manifest's text. Unparseable manifests are skipped.
pub(crate) fn discover(
    files: &BTreeSet<String>,
    manifests: &[String],
    read: &dyn Fn(&str) -> Option<String>,
) -> Vec<RustCrate> {
    let parsed: Vec<(String, Table)> = manifests
        .iter()
        .filter_map(|m| {
            let table = read(m)?.parse::<Table>().ok()?;
            Some((parent(m).to_string(), table))
        })
        .collect();
    // `[workspace.dependencies]` of every workspace root, for `x.workspace = true`.
    let workspaces: Vec<(&str, &Table)> = parsed
        .iter()
        .filter_map(|(dir, t)| Some((dir.as_str(), t.get("workspace")?.as_table()?)))
        .collect();
    parsed
        .iter()
        .filter_map(|(dir, table)| package(files, dir, table, &workspaces))
        .collect()
}

fn package(
    files: &BTreeSet<String>,
    dir: &str,
    table: &Table,
    workspaces: &[(&str, &Table)],
) -> Option<RustCrate> {
    let pkg = table.get("package")?.as_table()?;
    let name = pkg.get("name")?.as_str()?;
    let exists = |rel: &str| -> Option<String> {
        let p = normalize(&join_dir(dir, rel))?;
        files.contains(&p).then_some(p)
    };
    let auto = |key: &str| pkg.get(key).and_then(Value::as_bool).unwrap_or(true);

    let lib = table.get("lib").and_then(Value::as_table);
    let lib_root = match lib.and_then(|l| l.get("path")).and_then(Value::as_str) {
        Some(path) => exists(path),
        None => exists("src/lib.rs"),
    };
    let lib_name = lib
        .and_then(|l| l.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(name)
        .replace('-', "_");

    let mut other_roots: Vec<String> = Vec::new();
    for key in ["bin", "test", "example", "bench"] {
        let targets = table
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for t in targets {
            if let Some(p) = t.get("path").and_then(Value::as_str).and_then(exists) {
                other_roots.push(p);
            }
        }
    }
    let src = join_dir(dir, "src");
    if auto("autobins") {
        other_roots.extend(exists("src/main.rs"));
        other_roots.extend(conventional(files, &join_dir(&src, "bin")));
    }
    for (key, sub) in [
        ("autotests", "tests"),
        ("autoexamples", "examples"),
        ("autobenches", "benches"),
    ] {
        if auto(key) {
            other_roots.extend(conventional(files, &join_dir(dir, sub)));
        }
    }
    match pkg.get("build") {
        Some(Value::String(path)) => other_roots.extend(exists(path)),
        Some(Value::Boolean(false)) => {}
        _ => other_roots.extend(exists("build.rs")),
    }
    let mut seen = BTreeSet::new();
    other_roots.retain(|r| Some(r) != lib_root.as_ref() && seen.insert(r.clone()));

    let workspace = workspaces
        .iter()
        .filter(|(ws, _)| ws.is_empty() || *ws == dir || dir.starts_with(&format!("{ws}/")))
        .max_by_key(|(ws, _)| ws.len());
    Some(RustCrate {
        dir: dir.to_string(),
        lib_name,
        lib_root,
        other_roots,
        path_deps: path_deps(dir, table, workspace.copied()),
    })
}

/// `<dir>/*.rs` and `<dir>/*/main.rs`: Cargo's target auto-discovery.
fn conventional(files: &BTreeSet<String>, dir: &str) -> Vec<String> {
    let prefix = format!("{dir}/");
    files
        .range(prefix.clone()..)
        .take_while(|f| f.starts_with(&prefix))
        .filter(|f| {
            let rest = &f[prefix.len()..];
            match rest.split_once('/') {
                None => rest.ends_with(".rs"),
                Some((_, tail)) => tail == "main.rs",
            }
        })
        .cloned()
        .collect()
}

/// Every dependency table of a manifest: `[dependencies]`, `[dev-dependencies]`,
/// `[build-dependencies]` and their `[target.<cfg>.*]` variants.
fn dependency_tables(table: &Table) -> Vec<&Table> {
    const KEYS: [&str; 5] = [
        "dependencies",
        "dev-dependencies",
        "dev_dependencies",
        "build-dependencies",
        "build_dependencies",
    ];
    let mut out: Vec<&Table> = KEYS
        .iter()
        .filter_map(|k| table.get(*k)?.as_table())
        .collect();
    if let Some(targets) = table.get("target").and_then(Value::as_table) {
        for cfg in targets.values().filter_map(Value::as_table) {
            out.extend(KEYS.iter().filter_map(|k| cfg.get(*k)?.as_table()));
        }
    }
    out
}

fn path_deps(
    dir: &str,
    table: &Table,
    workspace: Option<(&str, &Table)>,
) -> BTreeMap<String, String> {
    let ws_deps =
        workspace.and_then(|(ws_dir, ws)| Some((ws_dir, ws.get("dependencies")?.as_table()?)));
    let mut out = BTreeMap::new();
    for deps in dependency_tables(table) {
        for (alias, spec) in deps {
            let Some(spec) = spec.as_table() else {
                continue;
            };
            let from_workspace = spec
                .get("workspace")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let target = if from_workspace {
                ws_deps.and_then(|(ws_dir, wd)| {
                    let path = wd.get(alias)?.as_table()?.get("path")?.as_str()?;
                    normalize(&join_dir(ws_dir, path))
                })
            } else {
                spec.get("path")
                    .and_then(Value::as_str)
                    .and_then(|p| normalize(&join_dir(dir, p)))
            };
            if let Some(target) = target {
                out.insert(alias.replace('-', "_"), target);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crates(files: &[(&str, &str)]) -> Vec<RustCrate> {
        let map: BTreeMap<String, String> = files
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect();
        let manifests: Vec<String> = map
            .keys()
            .filter(|k| k.ends_with("Cargo.toml"))
            .cloned()
            .collect();
        discover(&map.keys().cloned().collect(), &manifests, &|p| {
            map.get(p).cloned()
        })
    }

    #[test]
    fn explicit_target_paths_and_conventions_are_discovered() {
        let got = crates(&[
            (
                "Cargo.toml",
                "[package]\nname = \"rip-grep\"\nautotests = false\n[[bin]]\nname = \"rg\"\npath = \"crates/core/main.rs\"\n\
                 [[test]]\nname = \"it\"\npath = \"tests/tests.rs\"\n[workspace]\nmembers = [\"crates/a\"]\n",
            ),
            ("build.rs", ""),
            ("crates/core/main.rs", ""),
            ("tests/tests.rs", ""),
            ("tests/util.rs", ""),
            ("examples/demo.rs", ""),
            ("examples/big/main.rs", ""),
            ("examples/big/part.rs", ""),
        ]);
        assert_eq!(
            got,
            vec![RustCrate {
                dir: String::new(),
                lib_name: "rip_grep".into(),
                lib_root: None,
                other_roots: vec![
                    "crates/core/main.rs".into(),
                    "tests/tests.rs".into(),
                    "examples/big/main.rs".into(),
                    "examples/demo.rs".into(),
                    "build.rs".into(),
                ],
                path_deps: BTreeMap::new(),
            }]
        );
    }

    #[test]
    fn lib_path_name_and_path_dependencies_are_read() {
        let got = crates(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"a\", \"b\"]\n[workspace.dependencies]\nb-core = { path = \"b\" }\n",
            ),
            (
                "a/Cargo.toml",
                "[package]\nname = \"a\"\nbuild = false\n[dependencies]\nb-core.workspace = true\n\
                 serde = \"1\"\n[target.'cfg(unix)'.dev-dependencies]\nhelper = { package = \"h\", path = \"../h\" }\n",
            ),
            ("a/src/main.rs", ""),
            ("a/build.rs", ""),
            (
                "b/Cargo.toml",
                "[package]\nname = \"b-core\"\n[lib]\nname = \"bcore\"\npath = \"lib.rs\"\n",
            ),
            ("b/lib.rs", ""),
            ("b/src/main.rs", ""),
            ("broken/Cargo.toml", "[package\nname = "),
        ]);
        assert_eq!(got.len(), 2);
        let a = &got[0];
        assert_eq!(a.lib_root, None);
        assert_eq!(a.other_roots, vec!["a/src/main.rs".to_string()]);
        assert_eq!(
            a.path_deps,
            BTreeMap::from([
                ("b_core".to_string(), "b".to_string()),
                ("helper".to_string(), "h".to_string()),
            ])
        );
        let b = &got[1];
        assert_eq!(b.lib_name, "bcore");
        assert_eq!(b.lib_root.as_deref(), Some("b/lib.rs"));
        assert_eq!(b.other_roots, vec!["b/src/main.rs".to_string()]);
    }
}
