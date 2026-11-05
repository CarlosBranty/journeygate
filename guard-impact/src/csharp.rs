//! C# plugin: facts extracted from the syntax tree (type declarations, using
//! directives, type-name references) and their resolution through a symbol
//! table bounded by the MSBuild project model (`.csproj` SDK, implicit
//! usings, `<Using>`, `ProjectReference`, `<Compile Include>`), following C#'s
//! name lookup order.
//!
//! C# has no file imports. File A depends on file B when A names a type that
//! B declares; a partial type is declared by every file holding a part, and
//! a file of top-level statements declares the compiler's `Program`.

use crate::extract::RawImport;
use crate::resolve::{join_dir, normalize, parent};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

/// One C# fact of a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CsFact {
    /// A type declaration, fully qualified: `Shop.Orders.Order.Line`.
    Type(String),
    Using(CsUsing),
    Ref(CsRef),
}

/// A using directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CsUsing {
    /// Namespace the directive is declared in (`[]` = compilation unit).
    pub(crate) scope: Vec<String>,
    pub(crate) kind: UsingKind,
    /// The namespace or type it names, generic arguments dropped.
    pub(crate) target: Vec<String>,
    /// `global using`: applies to every file of the project.
    pub(crate) global: bool,
    /// The target is fully qualified (`global::A.B`, or an MSBuild `<Using>`
    /// item); otherwise it is resolved from the namespace around the directive.
    pub(crate) rooted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsingKind {
    /// `using A.B;`
    Namespace,
    /// `using static A.B.C;`
    Static,
    /// `using X = A.B;` (`X` is the alias).
    Alias(String),
}

/// A name in a type position (or an expression that may name a type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CsRef {
    /// Dotted name as written, generic arguments dropped: `Order.Line`.
    pub(crate) name: Vec<String>,
    /// Namespace around the reference, outermost first.
    pub(crate) namespace: Vec<String>,
    /// Type declarations around the reference, outermost first.
    pub(crate) types: Vec<String>,
    /// An attribute name: `Foo` also means `FooAttribute`.
    pub(crate) attribute: bool,
    /// `global::A.B` (or another `alias::`): looked up from the global namespace only.
    pub(crate) global: bool,
}

const TYPE_DECLARATIONS: [&str; 6] = [
    "class_declaration",
    "struct_declaration",
    "interface_declaration",
    "enum_declaration",
    "record_declaration",
    "delegate_declaration",
];

fn text<'a>(node: Node, bytes: &'a [u8]) -> &'a str {
    node.utf8_text(bytes).unwrap_or("")
}

/// Turns the captures of one query match into facts (`@declare`, `@using`,
/// `@program`, `@type`, `@attribute`, `@expr`; see `languages/csharp/imports.scm`).
pub(crate) fn collect(caps: &[(&str, Node)], bytes: &[u8], out: &mut Vec<RawImport>) {
    for (capture, node) in caps {
        match *capture {
            "declare" => {
                let scope = Scope::of(*node, bytes);
                let mut full = scope.namespace;
                full.extend(scope.types);
                out.push(RawImport::CSharp(CsFact::Type(full.join("."))));
            }
            // Top-level statements: the compiler's global `Program`.
            "program" => out.push(RawImport::CSharp(CsFact::Type("Program".to_string()))),
            "using" => {
                if let Some(using) = using_directive(*node, bytes) {
                    out.push(RawImport::CSharp(CsFact::Using(using)));
                }
            }
            "type" | "attribute" | "expr" => {
                let names = match *capture {
                    "type" => type_names(*node, bytes),
                    _ => dotted(*node, bytes).into_iter().collect(),
                };
                if names.is_empty() {
                    continue;
                }
                let scope = Scope::of(*node, bytes);
                for (name, global) in names {
                    // `T` of `class Box<T>` / `void M<T>()` is no type.
                    if !global && scope.type_params.contains(&name[0]) {
                        continue;
                    }
                    out.push(RawImport::CSharp(CsFact::Ref(CsRef {
                        name,
                        namespace: scope.namespace.clone(),
                        types: scope.types.clone(),
                        attribute: *capture == "attribute",
                        global,
                    })));
                }
            }
            _ => {}
        }
    }
}

/// Namespace, enclosing types and visible type parameters of a node, read
/// from its ancestors.
struct Scope {
    namespace: Vec<String>,
    types: Vec<String>,
    type_params: BTreeSet<String>,
}

impl Scope {
    fn of(node: Node, bytes: &[u8]) -> Scope {
        let mut namespaces: Vec<Vec<String>> = Vec::new();
        let mut types: Vec<String> = Vec::new();
        let mut type_params = BTreeSet::new();
        let mut current = node.parent();
        while let Some(n) = current {
            let kind = n.kind();
            if TYPE_DECLARATIONS.contains(&kind) {
                if let Some(name) = n.child_by_field_name("name") {
                    types.push(text(name, bytes).to_string());
                }
            }
            if TYPE_DECLARATIONS.contains(&kind)
                || matches!(kind, "method_declaration" | "local_function_statement")
            {
                let mut cursor = n.walk();
                for list in n
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "type_parameter_list")
                {
                    let mut inner = list.walk();
                    for p in list.named_children(&mut inner) {
                        if let Some(name) = p.child_by_field_name("name") {
                            type_params.insert(text(name, bytes).to_string());
                        }
                    }
                }
            }
            match kind {
                "namespace_declaration" => {
                    if let Some((name, _)) = n
                        .child_by_field_name("name")
                        .and_then(|name| dotted(name, bytes))
                    {
                        namespaces.push(name);
                    }
                }
                // A file-scoped namespace is a sibling of the declarations
                // after it, not their parent.
                "compilation_unit" => {
                    let mut cursor = n.walk();
                    let file_scoped = n
                        .named_children(&mut cursor)
                        .filter(|c| c.kind() == "file_scoped_namespace_declaration")
                        .find(|c| c.start_byte() < node.start_byte());
                    if let Some((name, _)) = file_scoped
                        .and_then(|c| c.child_by_field_name("name"))
                        .and_then(|name| dotted(name, bytes))
                    {
                        namespaces.push(name);
                    }
                }
                _ => {}
            }
            current = n.parent();
        }
        types.reverse();
        Scope {
            namespace: namespaces.into_iter().rev().flatten().collect(),
            types,
            type_params,
        }
    }
}

fn using_directive(node: Node, bytes: &[u8]) -> Option<CsUsing> {
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    let alias = node.child_by_field_name("name");
    let target = children
        .iter()
        .find(|c| c.is_named() && Some(**c) != alias)
        .and_then(|t| dotted(*t, bytes))?;
    let has = |token: &str| children.iter().any(|c| !c.is_named() && c.kind() == token);
    let kind = match alias {
        Some(a) => UsingKind::Alias(text(a, bytes).to_string()),
        None if has("static") => UsingKind::Static,
        None => UsingKind::Namespace,
    };
    Some(CsUsing {
        scope: Scope::of(node, bytes).namespace,
        kind,
        target: target.0,
        global: has("global"),
        rooted: target.1,
    })
}

/// The type names a type node mentions: the type itself, or the element type
/// of an array / nullable / pointer / ref type. Generic arguments, tuple
/// elements and function-pointer signatures are captured on their own.
fn type_names(node: Node, bytes: &[u8]) -> Vec<(Vec<String>, bool)> {
    match node.kind() {
        "array_type" | "nullable_type" | "pointer_type" | "ref_type" | "scoped_type" => node
            .child_by_field_name("type")
            .map(|t| type_names(t, bytes))
            .unwrap_or_default(),
        _ => dotted(node, bytes).into_iter().collect(),
    }
}

/// The dotted name of an identifier, generic name, qualified name, alias
/// qualified name or member access chain, generic arguments dropped. The
/// flag is set for a name rooted at `global::` (or another extern alias).
/// Anything else (`this.x`, calls, literals, predefined types) is `None`.
fn dotted(node: Node, bytes: &[u8]) -> Option<(Vec<String>, bool)> {
    match node.kind() {
        "identifier" => Some((vec![text(node, bytes).to_string()], false)),
        "generic_name" => {
            let name = node.named_child(0).filter(|n| n.kind() == "identifier")?;
            Some((vec![text(name, bytes).to_string()], false))
        }
        "alias_qualified_name" => {
            let (name, _) = dotted(node.child_by_field_name("name")?, bytes)?;
            Some((name, true))
        }
        "qualified_name" | "member_access_expression" => {
            let left = match node.kind() {
                "qualified_name" => node.child_by_field_name("qualifier")?,
                _ => node.child_by_field_name("expression")?,
            };
            let (mut name, global) = dotted(left, bytes)?;
            let (right, _) = dotted(node.child_by_field_name("name")?, bytes)?;
            name.extend(right);
            Some((name, global))
        }
        _ => None,
    }
}

// ------------------------------------------------------------ MSBuild

/// The SDK's implicit global usings (`ImplicitUsings` enabled).
const BASE_USINGS: [&str; 7] = [
    "System",
    "System.Collections.Generic",
    "System.IO",
    "System.Linq",
    "System.Net.Http",
    "System.Threading",
    "System.Threading.Tasks",
];
const HOSTING_USINGS: [&str; 4] = [
    "Microsoft.Extensions.Configuration",
    "Microsoft.Extensions.DependencyInjection",
    "Microsoft.Extensions.Hosting",
    "Microsoft.Extensions.Logging",
];
const WEB_USINGS: [&str; 5] = [
    "System.Net.Http.Json",
    "Microsoft.AspNetCore.Builder",
    "Microsoft.AspNetCore.Hosting",
    "Microsoft.AspNetCore.Http",
    "Microsoft.AspNetCore.Routing",
];
const ASPIRE_USINGS: [&str; 2] = ["Aspire.Hosting", "Aspire.Hosting.ApplicationModel"];
const MAUI_USINGS: [&str; 17] = [
    "Microsoft.Extensions.DependencyInjection",
    "Microsoft.Maui",
    "Microsoft.Maui.Controls",
    "Microsoft.Maui.Controls.Hosting",
    "Microsoft.Maui.Controls.Xaml",
    "Microsoft.Maui.Graphics",
    "Microsoft.Maui.Hosting",
    "Microsoft.Maui.ApplicationModel",
    "Microsoft.Maui.ApplicationModel.Communication",
    "Microsoft.Maui.ApplicationModel.DataTransfer",
    "Microsoft.Maui.Authentication",
    "Microsoft.Maui.Devices",
    "Microsoft.Maui.Devices.Sensors",
    "Microsoft.Maui.Media",
    "Microsoft.Maui.Networking",
    "Microsoft.Maui.Storage",
    "Microsoft.Maui.Accessibility",
];

/// A `<Using>` item.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UsingItem {
    include: String,
    alias: Option<String>,
    is_static: bool,
    remove: bool,
}

/// What guard-impact reads from one project: its `.csproj`, after the
/// `Directory.Build.props` chain and before `Directory.Build.targets`.
#[derive(Debug, Default, PartialEq, Eq)]
struct MsBuildProject {
    /// SDK names without version (`Microsoft.NET.Sdk.Web`).
    sdks: BTreeSet<String>,
    /// Unconditioned properties, last definition wins.
    props: BTreeMap<String, String>,
    usings: Vec<UsingItem>,
    /// `ProjectReference` includes, relative to the project directory.
    references: Vec<String>,
    /// `Compile` includes, relative to the project directory.
    compile: Vec<String>,
}

impl MsBuildProject {
    /// `csproj` with the build files around it (`manifests`: path -> text).
    fn load(csproj: &str, manifests: &BTreeMap<String, String>) -> Option<Self> {
        let text = manifests.get(csproj)?;
        let dir = parent(csproj);
        let mut project = MsBuildProject::default();
        for props in build_files(dir, "Directory.Build.props", manifests)
            .into_iter()
            .rev()
        {
            project.apply(props, false);
        }
        project.apply(text, true);
        for targets in build_files(dir, "Directory.Build.targets", manifests)
            .into_iter()
            .rev()
        {
            project.apply(targets, false);
        }
        Some(project)
    }

    fn apply(&mut self, text: &str, is_project: bool) {
        let Ok(doc) = roxmltree::Document::parse(text.trim_start_matches('\u{feff}')) else {
            return;
        };
        let root = doc.root_element();
        if is_project {
            if let Some(sdk) = root.attribute("Sdk") {
                self.sdks.extend(sdk_names(sdk));
            }
        }
        for group in root.children().filter(|n| n.is_element()) {
            match group.tag_name().name() {
                "Sdk" => self
                    .sdks
                    .extend(group.attribute("Name").map(sdk_names).into_iter().flatten()),
                "PropertyGroup" if group.attribute("Condition").is_none() => {
                    for p in group
                        .children()
                        .filter(|n| n.is_element() && n.attribute("Condition").is_none())
                    {
                        let name = p.tag_name().name().to_string();
                        let value: String = p
                            .descendants()
                            .filter(|n| n.is_text())
                            .filter_map(|n| n.text())
                            .collect();
                        let own = format!("$({name})");
                        let value = if value.contains(&own) {
                            value.replace(&own, self.props.get(&name).map_or("", String::as_str))
                        } else {
                            value
                        };
                        self.props.insert(name, value);
                    }
                }
                // Items are read even when conditioned: recall over precision.
                "ItemGroup" => {
                    for item in group.children().filter(|n| n.is_element()) {
                        let includes = item.attribute("Include").map(plain_values);
                        match item.tag_name().name() {
                            "Using" => {
                                let remove = item.attribute("Remove").map(plain_values);
                                for (value, remove) in includes
                                    .into_iter()
                                    .flatten()
                                    .map(|v| (v, false))
                                    .chain(remove.into_iter().flatten().map(|v| (v, true)))
                                {
                                    self.usings.push(UsingItem {
                                        include: value,
                                        alias: item.attribute("Alias").map(str::to_string),
                                        is_static: item
                                            .attribute("Static")
                                            .is_some_and(|s| s.trim().eq_ignore_ascii_case("true")),
                                        remove,
                                    });
                                }
                            }
                            "ProjectReference" => {
                                self.references.extend(includes.into_iter().flatten())
                            }
                            "Compile" => self.compile.extend(includes.into_iter().flatten()),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn flag(&self, name: &str) -> bool {
        self.props
            .get(name)
            .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "enable"))
    }

    /// The project's global usings: the SDK's implicit usings, then the
    /// `<Using>` items (a `Remove` drops an earlier include).
    fn global_usings(&self) -> Vec<CsUsing> {
        let mut list: Vec<(UsingKind, String)> = Vec::new();
        if self.flag("ImplicitUsings") {
            let mut ns: Vec<&str> = BASE_USINGS.to_vec();
            if self.sdks.contains("Microsoft.NET.Sdk.Web") {
                ns.extend(WEB_USINGS);
                ns.extend(HOSTING_USINGS);
            }
            if self.sdks.contains("Microsoft.NET.Sdk.Worker") {
                ns.extend(HOSTING_USINGS);
            }
            if self.sdks.contains("Aspire.AppHost.Sdk") {
                ns.extend(ASPIRE_USINGS);
            }
            if self.flag("UseMaui") {
                ns.extend(MAUI_USINGS);
            }
            list.extend(
                ns.into_iter()
                    .map(|n| (UsingKind::Namespace, n.to_string())),
            );
        }
        if self.sdks.contains("MSTest.Sdk") {
            list.push((
                UsingKind::Namespace,
                "Microsoft.VisualStudio.TestTools.UnitTesting".to_string(),
            ));
        }
        for u in &self.usings {
            if u.remove {
                list.retain(|(_, target)| *target != u.include);
                continue;
            }
            let kind = match (&u.alias, u.is_static) {
                (Some(alias), _) => UsingKind::Alias(alias.clone()),
                (None, true) => UsingKind::Static,
                (None, false) => UsingKind::Namespace,
            };
            list.push((kind, u.include.clone()));
        }
        let mut out: Vec<CsUsing> = Vec::new();
        for (kind, target) in list {
            let using = CsUsing {
                scope: Vec::new(),
                kind,
                target: target.split('.').map(|s| s.trim().to_string()).collect(),
                global: true,
                rooted: true,
            };
            if !out.contains(&using) {
                out.push(using);
            }
        }
        out
    }
}

/// `Name/Version` entries of an `Sdk` attribute, without versions.
fn sdk_names(attr: &str) -> Vec<String> {
    attr.split(';')
        .filter_map(|s| s.split('/').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The values of an `Include` / `Remove` list, with `/` separators; values
/// holding properties, item lists or wildcards are dropped.
fn plain_values(attr: &str) -> Vec<String> {
    attr.split(';')
        .map(str::trim)
        .filter(|v| !v.is_empty() && !v.contains("$(") && !v.contains("@(") && !v.contains('*'))
        .map(|v| v.replace('\\', "/"))
        .collect()
}

/// The `name` build file nearest at or above `dir`, then the ones it chains
/// to with `GetPathOfFileAbove`, nearest first.
fn build_files<'a>(dir: &str, name: &str, manifests: &'a BTreeMap<String, String>) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut d = dir;
    loop {
        if let Some(text) = manifests.get(&join_dir(d, name)) {
            out.push(text.as_str());
            if !text.contains("GetPathOfFileAbove") {
                break;
            }
        }
        if d.is_empty() {
            break;
        }
        d = parent(d);
    }
    out
}

// ------------------------------------------------------------ resolution

/// C# symbol table plus the project model: global usings and the
/// compilations (`ProjectReference` closures) that bound name lookup.
#[derive(Debug, Default)]
pub(crate) struct CsIndex {
    /// Fully qualified type name -> files declaring it (every partial part).
    types: BTreeMap<String, BTreeSet<String>>,
    /// Every proper prefix of a declared type name: the namespaces (and
    /// enclosing types) the repository declares.
    namespaces: BTreeSet<String>,
    /// The last segment of every declared type name (`Line` of
    /// `Shop.Orders.Order.Line`): a name none of whose segments is here
    /// cannot resolve, except through an alias.
    simple_names: BTreeSet<String>,
    /// Directories holding a `.csproj`: the projects.
    projects: BTreeSet<String>,
    /// Compilation (`None` = files under no `.csproj`) -> its global usings.
    global_usings: BTreeMap<Option<String>, Vec<CsUsing>>,
    /// Project -> itself plus every project it references, transitively.
    visible: BTreeMap<String, BTreeSet<String>>,
    /// C# file -> the compilations it is part of: its own project plus the
    /// projects linking it with `<Compile Include>`.
    compilations: BTreeMap<String, Vec<Option<String>>>,
}

impl CsIndex {
    /// `raw` holds the raw imports of every C# file; `manifests` the MSBuild
    /// files by path.
    pub(crate) fn build(
        files: &BTreeSet<String>,
        raw: &BTreeMap<String, Vec<RawImport>>,
        manifests: &BTreeMap<String, String>,
    ) -> Self {
        let mut index = CsIndex {
            projects: files
                .iter()
                .filter(|f| f.ends_with(".csproj"))
                .map(|f| parent(f).to_string())
                .collect(),
            ..CsIndex::default()
        };
        let mut references: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut linked: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for csproj in files.iter().filter(|f| f.ends_with(".csproj")) {
            let dir = parent(csproj).to_string();
            let Some(project) = MsBuildProject::load(csproj, manifests) else {
                continue;
            };
            for r in &project.references {
                if let Some(path) = normalize(&join_dir(&dir, r)) {
                    if path.ends_with(".csproj") && files.contains(&path) {
                        references
                            .entry(dir.clone())
                            .or_default()
                            .insert(parent(&path).to_string());
                    }
                }
            }
            for c in &project.compile {
                if let Some(path) = normalize(&join_dir(&dir, c)) {
                    linked.entry(path).or_default().insert(dir.clone());
                }
            }
            index
                .global_usings
                .entry(Some(dir))
                .or_default()
                .extend(project.global_usings());
        }
        for p in &index.projects {
            let mut seen = BTreeSet::from([p.clone()]);
            let mut todo = vec![p.clone()];
            while let Some(q) = todo.pop() {
                for r in references.get(&q).into_iter().flatten() {
                    if seen.insert(r.clone()) {
                        todo.push(r.clone());
                    }
                }
            }
            index.visible.insert(p.clone(), seen);
        }
        for (file, imports) in raw {
            let own = index.project_of(file);
            let mut comps: Vec<Option<String>> = linked
                .get(file)
                .into_iter()
                .flatten()
                .map(|p| Some(p.clone()))
                .collect();
            if (own.is_some() || comps.is_empty()) && !comps.contains(&own) {
                comps.insert(0, own);
            }
            for fact in imports {
                match fact {
                    RawImport::CSharp(CsFact::Type(name)) => {
                        let parts: Vec<&str> = name.split('.').collect();
                        for k in 1..parts.len() {
                            index.namespaces.insert(parts[..k].join("."));
                        }
                        if let Some(last) = parts.last() {
                            index.simple_names.insert(last.to_string());
                        }
                        index
                            .types
                            .entry(name.clone())
                            .or_default()
                            .insert(file.clone());
                    }
                    RawImport::CSharp(CsFact::Using(u)) if u.global => {
                        for c in &comps {
                            index
                                .global_usings
                                .entry(c.clone())
                                .or_default()
                                .push(u.clone());
                        }
                    }
                    _ => {}
                }
            }
            index.compilations.insert(file.clone(), comps);
        }
        index
    }

    /// The innermost directory at or above `file` holding a `.csproj`.
    fn project_of(&self, file: &str) -> Option<String> {
        let mut dir = parent(file);
        loop {
            if self.projects.contains(dir) {
                return Some(dir.to_string());
            }
            if dir.is_empty() {
                return None;
            }
            dir = parent(dir);
        }
    }

    fn compilations_of(&self, file: &str) -> Vec<Option<String>> {
        self.compilations
            .get(file)
            .cloned()
            .unwrap_or_else(|| vec![self.project_of(file)])
    }

    /// Whether a type declared in `file` is part of compilation `view`
    /// (`None`: no filter). Files under no project are visible everywhere.
    fn is_visible(&self, view: Option<&str>, file: &str) -> bool {
        let Some(project) = view else {
            return true;
        };
        let visible = self.visible.get(project);
        let reaches = |c: &Option<String>| match c {
            None => true,
            Some(q) => q == project || visible.is_some_and(|v| v.contains(q)),
        };
        match self.compilations.get(file) {
            Some(comps) => comps.iter().any(reaches),
            None => reaches(&self.project_of(file)),
        }
    }

    /// Files declaring the types `facts` (the facts of `from`) reference.
    /// Each compilation of `from` resolves on its own; a name with no hit in
    /// the compilation is retried repository-wide (recall over precision).
    pub(crate) fn resolve(&self, from: &str, facts: &[&CsFact]) -> BTreeSet<String> {
        let own: Vec<CsUsing> = facts
            .iter()
            .filter_map(|f| match f {
                CsFact::Using(u) if !u.global => Some(self.bind_using(u)),
                _ => None,
            })
            .collect();
        let mut out = BTreeSet::new();
        for comp in self.compilations_of(from) {
            let mut usings: Vec<&CsUsing> = own.iter().collect();
            usings.extend(self.global_usings.get(&comp).into_iter().flatten());
            let view = comp.as_deref();
            let with_fallback = |find: &dyn Fn(Option<&str>) -> BTreeSet<String>| {
                let hits = find(view);
                if hits.is_empty() && view.is_some() {
                    find(None)
                } else {
                    hits
                }
            };
            for fact in facts {
                match fact {
                    // `using static T;` and `using X = T;` name the type `T`.
                    CsFact::Using(u) if !matches!(u.kind, UsingKind::Namespace) => {
                        let bound = self.bind_using(u);
                        out.extend(with_fallback(&|v| self.lookup(v, &[], &bound.target)));
                    }
                    CsFact::Ref(r) if self.may_resolve(r, &usings) => {
                        out.extend(with_fallback(&|v| self.resolve_ref(r, &usings, v)));
                    }
                    _ => {}
                }
            }
        }
        out
    }

    /// Whether `r` can name a repository type at all: one of its segments
    /// (or `<last>Attribute` for an attribute) is the last segment of a
    /// declared type, or its first segment is an alias. Most references
    /// (BCL and package types) fail this cheap test and skip the lookup.
    fn may_resolve(&self, r: &CsRef, usings: &[&CsUsing]) -> bool {
        r.name.iter().any(|seg| self.simple_names.contains(seg))
            || (r.attribute
                && r.name
                    .last()
                    .is_some_and(|l| self.simple_names.contains(&format!("{l}Attribute"))))
            || usings
                .iter()
                .any(|u| matches!(&u.kind, UsingKind::Alias(a) if *a == r.name[0]))
    }

    /// A using directive with its target resolved from the namespace around
    /// it, innermost first: `using Orders;` in `namespace Shop` names
    /// `Shop.Orders` when the repository declares it. Unknown targets (the
    /// BCL, packages) stay as written.
    fn bind_using(&self, u: &CsUsing) -> CsUsing {
        if u.rooted || u.scope.is_empty() {
            return u.clone();
        }
        for j in (0..=u.scope.len()).rev() {
            let mut candidate = u.scope[..j].to_vec();
            candidate.extend_from_slice(&u.target);
            let key = candidate.join(".");
            let found = match u.kind {
                UsingKind::Namespace => self.namespaces.contains(&key),
                UsingKind::Static => self.types.contains_key(&key),
                UsingKind::Alias(_) => {
                    self.types.contains_key(&key) || self.namespaces.contains(&key)
                }
            };
            if found {
                return CsUsing {
                    target: candidate,
                    ..u.clone()
                };
            }
        }
        u.clone()
    }

    /// C# name lookup: nested types of the enclosing types (innermost
    /// first), then each namespace from the innermost to the global one,
    /// where the namespace's own types come before the using directives
    /// declared in it, and a using alias before the using namespaces. The
    /// first level with any hit wins; every hit of that level counts (an
    /// ambiguous name reaches every candidate). `view` is the compilation.
    fn resolve_ref(&self, r: &CsRef, usings: &[&CsUsing], view: Option<&str>) -> BTreeSet<String> {
        let mut variants = vec![r.name.clone()];
        if r.attribute {
            let mut long = r.name.clone();
            if let Some(last) = long.last_mut() {
                last.push_str("Attribute");
            }
            variants.insert(0, long);
        }
        let at = |prefix: &[String]| -> BTreeSet<String> {
            variants
                .iter()
                .flat_map(|v| self.lookup(view, prefix, v))
                .collect()
        };
        if r.global {
            return at(&[]);
        }
        for i in (1..=r.types.len()).rev() {
            let mut prefix = r.namespace.clone();
            prefix.extend_from_slice(&r.types[..i]);
            let hits = at(&prefix);
            if !hits.is_empty() {
                return hits;
            }
        }
        for j in (0..=r.namespace.len()).rev() {
            let scope = &r.namespace[..j];
            let hits = at(scope);
            if !hits.is_empty() {
                return hits;
            }
            let level: Vec<&CsUsing> = usings
                .iter()
                .copied()
                .filter(|u| u.scope == scope)
                .collect();
            // An alias of the name binds it, even to a type outside the
            // repository: the using namespaces of the level are not consulted.
            let aliases: Vec<&CsUsing> = level
                .iter()
                .copied()
                .filter(|u| matches!(&u.kind, UsingKind::Alias(a) if variants.iter().any(|v| v[0] == *a)))
                .collect();
            let aliased = !aliases.is_empty();
            let candidates = if aliased { aliases } else { level };
            let hits: BTreeSet<String> = candidates
                .iter()
                .flat_map(|u| variants.iter().flat_map(|v| self.through_using(u, v, view)))
                .collect();
            if !hits.is_empty() || aliased {
                return hits;
            }
        }
        BTreeSet::new()
    }

    /// `name` seen through one using directive.
    fn through_using(&self, u: &CsUsing, name: &[String], view: Option<&str>) -> BTreeSet<String> {
        match &u.kind {
            UsingKind::Alias(alias) if name[0] == *alias => {
                let mut full = u.target.clone();
                full.extend_from_slice(&name[1..]);
                self.lookup(view, &[], &full)
            }
            UsingKind::Alias(_) => BTreeSet::new(),
            // A using namespace (or using static) imports the types directly
            // inside it, not its nested namespaces.
            UsingKind::Namespace | UsingKind::Static => {
                let mut head = u.target.clone();
                head.push(name[0].clone());
                let visible = self
                    .types
                    .get(&head.join("."))
                    .is_some_and(|files| files.iter().any(|f| self.is_visible(view, f)));
                if visible {
                    self.lookup(view, &u.target, name)
                } else {
                    BTreeSet::new()
                }
            }
        }
    }

    /// Files of compilation `view` declaring `prefix.name`, or the longest
    /// prefix of `name` that is such a type: in `Pricing.Rate` or
    /// `Order.Missing` the rest names members.
    fn lookup(&self, view: Option<&str>, prefix: &[String], name: &[String]) -> BTreeSet<String> {
        for k in (1..=name.len()).rev() {
            let full: Vec<&str> = prefix
                .iter()
                .chain(&name[..k])
                .map(String::as_str)
                .collect();
            if let Some(files) = self.types.get(&full.join(".")) {
                let files: BTreeSet<String> = files
                    .iter()
                    .filter(|f| self.is_visible(view, f))
                    .cloned()
                    .collect();
                if !files.is_empty() {
                    return files;
                }
            }
        }
        BTreeSet::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(types: &[(&str, &str)], csproj: &[&str]) -> CsIndex {
        let mut idx = CsIndex::default();
        for (name, file) in types {
            idx.types
                .entry(name.to_string())
                .or_default()
                .insert(file.to_string());
        }
        idx.projects = csproj.iter().map(|d| d.to_string()).collect();
        idx
    }

    fn seg(s: &str) -> Vec<String> {
        if s.is_empty() {
            Vec::new()
        } else {
            s.split('.').map(str::to_string).collect()
        }
    }

    #[test]
    fn lookup_takes_the_longest_prefix_that_is_a_type() {
        let idx = index(&[("A.Order", "o.cs"), ("A.Order.Line", "l.cs")], &[]);
        let files = |p: &str, n: &str| {
            idx.lookup(None, &seg(p), &seg(n))
                .into_iter()
                .collect::<Vec<_>>()
        };
        assert_eq!(files("A", "Order.Line"), vec!["l.cs"]);
        assert_eq!(files("A", "Order.Line.Max"), vec!["l.cs"]);
        assert_eq!(files("A", "Order.Missing"), vec!["o.cs"]);
        assert!(files("", "Order").is_empty());
    }

    #[test]
    fn projects_are_the_innermost_csproj_directory() {
        let idx = index(&[], &["", "src/App"]);
        assert_eq!(idx.project_of("src/App/x/A.cs"), Some("src/App".into()));
        assert_eq!(idx.project_of("src/Lib/A.cs"), Some(String::new()));
        assert_eq!(index(&[], &[]).project_of("A.cs"), None);
    }

    #[test]
    fn attribute_names_try_the_attribute_suffix_first_and_keep_both() {
        let idx = index(&[("N.Mark", "m.cs"), ("N.MarkAttribute", "ma.cs")], &[]);
        let r = CsRef {
            name: seg("Mark"),
            namespace: seg("N"),
            types: Vec::new(),
            attribute: true,
            global: false,
        };
        assert_eq!(
            idx.resolve_ref(&r, &[], None)
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["m.cs", "ma.cs"]
        );
    }

    #[test]
    fn using_namespace_does_not_import_nested_namespaces() {
        let idx = index(&[("A.Sub.Thing", "t.cs")], &[]);
        let using = CsUsing {
            scope: Vec::new(),
            kind: UsingKind::Namespace,
            target: seg("A"),
            global: false,
            rooted: false,
        };
        assert!(
            idx.through_using(&using, &seg("Sub.Thing"), None)
                .is_empty()
        );
        let deeper = CsUsing {
            target: seg("A.Sub"),
            ..using
        };
        assert_eq!(
            idx.through_using(&deeper, &seg("Thing"), None)
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["t.cs"]
        );
    }

    #[test]
    fn msbuild_project_reads_the_build_file_chain_and_using_items() {
        let files: BTreeMap<String, String> = [
            (
                "Directory.Build.props",
                r#"<Project><PropertyGroup><ImplicitUsings>enable</ImplicitUsings></PropertyGroup></Project>"#,
            ),
            (
                "src/Directory.Build.props",
                r#"<Project><Import Project="$([MSBuild]::GetPathOfFileAbove('Directory.Build.props', '$(MSBuildThisFileDirectory)../'))" />
                   <ItemGroup><Using Include="Shop.Shared" /></ItemGroup></Project>"#,
            ),
            (
                "src/App/App.csproj",
                "\u{feff}<Project><Sdk Name=\"Aspire.AppHost.Sdk\" Version=\"9.0.0\" />
                 <PropertyGroup Condition=\"'$(X)' == ''\"><ImplicitUsings>disable</ImplicitUsings></PropertyGroup>
                 <ItemGroup><Using Remove=\"System.IO\" /><Using Include=\"Shop.Money\" Static=\"true\" />
                 <Using Include=\"Shop.Orders.Order\" Alias=\"O\" />
                 <ProjectReference Include=\"..\\Lib\\Lib.csproj;$(Other)\" /></ItemGroup></Project>",
            ),
        ]
        .into_iter()
        .map(|(p, t)| (p.to_string(), t.to_string()))
        .collect();
        let project = MsBuildProject::load("src/App/App.csproj", &files).expect("loads");
        assert_eq!(project.references, vec!["../Lib/Lib.csproj"]);
        let usings: Vec<(String, UsingKind)> = project
            .global_usings()
            .into_iter()
            .map(|u| (u.target.join("."), u.kind))
            .collect();
        let names: Vec<&str> = usings.iter().map(|(t, _)| t.as_str()).collect();
        assert!(
            names.contains(&"Aspire.Hosting"),
            "the `<Sdk>` element names the SDK"
        );
        assert!(
            names.contains(&"Shop.Shared"),
            "the chained props file applies"
        );
        assert!(
            names.contains(&"System"),
            "a conditioned PropertyGroup is skipped"
        );
        assert!(
            !names.contains(&"System.IO"),
            "`Remove` drops an implicit using"
        );
        assert!(usings.contains(&("Shop.Money".to_string(), UsingKind::Static)));
        assert!(usings.contains(&(
            "Shop.Orders.Order".to_string(),
            UsingKind::Alias("O".into())
        )));
    }
}
