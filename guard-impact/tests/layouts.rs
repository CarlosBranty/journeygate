//! Project layouts the import resolvers must model like the toolchains do:
//! Python source roots, Cargo target paths and `#[path]`, TypeScript module
//! augmentation and tsconfig `paths` / `baseUrl` (with `extends`), C#
//! name lookup (namespaces, usings, `.csproj` project scopes), and PHP name
//! resolution with Composer autoload maps and static includes.
//! Every repository here is built in memory with `ImpactIndex::from_sources`.

use guard_core::config::parse_target;
use guard_impact::ImpactIndex;

fn index(language: &str, files: &[(&str, &str)]) -> ImpactIndex {
    let config = parse_target(&format!(
        "version = 1\n[app]\nname = \"t\"\nlanguages = [\"{language}\"]\n\
         base_url = \"http://localhost\"\n[areas.all]\ndescription = \"All\"\npaths = [\"**\"]\n"
    ))
    .expect("valid config");
    ImpactIndex::from_sources(files.iter().copied(), &config).expect("index builds")
}

fn imports<'a>(idx: &'a ImpactIndex, file: &str) -> Vec<&'a str> {
    idx.imports_of(file)
}

// ------------------------------------------------------------------ python

#[test]
fn python_absolute_imports_inside_a_package_do_not_shadow_the_stdlib() {
    let idx = index(
        "python",
        &[
            ("rich/__init__.py", ""),
            ("rich/abc.py", ""),
            ("rich/json.py", ""),
            ("rich/text.py", ""),
            (
                "rich/console.py",
                "from abc import ABC\nimport json\nfrom rich import text\nfrom .abc import RichRenderable\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "rich/console.py"),
        vec!["rich/__init__.py", "rich/abc.py", "rich/text.py"],
        "`abc` and `json` are the stdlib; only the relative `.abc` and `rich.text` are repo files"
    );
}

#[test]
fn python_nested_packages_resolve_from_the_first_non_package_directory() {
    let idx = index(
        "python",
        &[
            ("pkg/__init__.py", ""),
            ("pkg/sub/__init__.py", ""),
            ("pkg/sub/util.py", ""),
            (
                "pkg/sub/m.py",
                "import util\nimport sub.util\nimport pkg.sub.util\n",
            ),
        ],
    );
    // Only `pkg.sub.util` is importable: the basedir of `pkg/sub/m.py` is the repo root.
    assert_eq!(imports(&idx, "pkg/sub/m.py"), vec!["pkg/sub/util.py"]);
}

#[test]
fn python_scripts_and_test_directories_import_their_siblings() {
    let idx = index(
        "python",
        &[
            ("tests/helpers.py", ""),
            ("tests/test_a.py", "import helpers\n"),
            ("scripts/tool/helper.py", ""),
            ("scripts/tool/run.py", "from helper import go\n"),
        ],
    );
    assert_eq!(imports(&idx, "tests/test_a.py"), vec!["tests/helpers.py"]);
    assert_eq!(
        imports(&idx, "scripts/tool/run.py"),
        vec!["scripts/tool/helper.py"]
    );
}

#[test]
fn python_src_layout_and_configured_roots_are_search_roots() {
    let idx = index(
        "python",
        &[
            (
                "pyproject.toml",
                "[tool.pytest.ini_options]\npythonpath = [\"lib\"]\n\n\
                 [tool.setuptools.packages.find]\nwhere = [\"python\"]\n",
            ),
            ("src/app/__init__.py", ""),
            ("src/app/core.py", ""),
            ("lib/shared.py", ""),
            ("python/ext/__init__.py", ""),
            ("python/ext/x.py", ""),
            (
                "tests/test_core.py",
                "from app import core\nimport shared\nimport ext.x\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "tests/test_core.py"),
        vec![
            "lib/shared.py",
            "python/ext/x.py",
            "src/app/__init__.py",
            "src/app/core.py",
        ]
    );
}

#[test]
fn python_setuptools_package_dir_is_a_search_root() {
    let idx = index(
        "python",
        &[
            (
                "pyproject.toml",
                "[tool.setuptools.package-dir]\n\"\" = \"code\"\n",
            ),
            ("code/mylib/__init__.py", ""),
            ("main.py", "import mylib\n"),
        ],
    );
    assert_eq!(imports(&idx, "main.py"), vec!["code/mylib/__init__.py"]);
}

// ------------------------------------------------------------------ rust

const RIPGREP_LIKE: &str = "[package]\nname = \"ripgrep\"\nbuild = \"build.rs\"\nautotests = false\n\n\
    [[bin]]\nname = \"rg\"\npath = \"crates/core/main.rs\"\n\n\
    [[test]]\nname = \"integration\"\npath = \"tests/tests.rs\"\n\n\
    [workspace]\nmembers = [\"crates/grep\"]\n\n\
    [dependencies]\ngrep = { version = \"0.4\", path = \"crates/grep\" }\n";

#[test]
fn rust_bin_and_test_paths_from_cargo_toml_are_crate_roots() {
    let idx = index(
        "rust",
        &[
            ("Cargo.toml", RIPGREP_LIKE),
            ("build.rs", ""),
            (
                "crates/core/main.rs",
                "mod flags;\nmod search;\nuse crate::flags::Args;\n",
            ),
            (
                "crates/core/flags/mod.rs",
                "mod parse;\npub use self::parse::parse;\n",
            ),
            (
                "crates/core/flags/parse.rs",
                "use crate::search::Searcher;\n",
            ),
            (
                "crates/core/search.rs",
                "use grep::Matcher;\nuse crate::flags::Args;\n",
            ),
            ("crates/grep/Cargo.toml", "[package]\nname = \"grep\"\n"),
            ("crates/grep/src/lib.rs", "pub mod matcher;\n"),
            ("crates/grep/src/matcher.rs", ""),
            ("tests/tests.rs", "mod util;\nmod feature;\n"),
            ("tests/util.rs", ""),
            ("tests/feature.rs", "use crate::util::Dir;\n"),
        ],
    );
    assert_eq!(
        imports(&idx, "crates/core/main.rs"),
        vec!["crates/core/flags/mod.rs", "crates/core/search.rs"]
    );
    assert_eq!(
        imports(&idx, "crates/core/flags/parse.rs"),
        vec!["crates/core/search.rs"]
    );
    assert_eq!(
        imports(&idx, "crates/core/search.rs"),
        vec!["crates/core/flags/mod.rs", "crates/grep/src/lib.rs"]
    );
    assert_eq!(imports(&idx, "tests/feature.rs"), vec!["tests/util.rs"]);
}

#[test]
fn rust_lib_path_and_lib_name_are_read_from_cargo_toml() {
    let idx = index(
        "rust",
        &[
            (
                "core/Cargo.toml",
                "[package]\nname = \"shop-core\"\n\n[lib]\nname = \"shopcore\"\npath = \"lib.rs\"\n",
            ),
            ("core/lib.rs", "pub mod money;\n"),
            ("core/money.rs", ""),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\n\n[dependencies]\nshop = { package = \"shop-core\", path = \"../core\" }\n",
            ),
            ("app/src/main.rs", "use shop::money::Money;\n"),
            ("core/tests/it.rs", "use shopcore::money;\n"),
        ],
    );
    assert_eq!(imports(&idx, "core/lib.rs"), vec!["core/money.rs"]);
    // A renamed path dependency, and the crate's own `[lib] name`.
    assert_eq!(imports(&idx, "app/src/main.rs"), vec!["core/money.rs"]);
    assert_eq!(imports(&idx, "core/tests/it.rs"), vec!["core/money.rs"]);
}

#[test]
fn rust_path_attributes_on_mod_declarations_are_honoured() {
    let idx = index(
        "rust",
        &[
            ("Cargo.toml", "[package]\nname = \"k\"\n"),
            (
                "src/lib.rs",
                "#[path = \"platform/unix_impl.rs\"]\nmod sys;\n\
                 #[cfg(test)]\n#[path = \"../testdata/fixtures.rs\"]\nmod fixtures;\n\
                 mod outer {\n    #[path = \"inner_impl.rs\"]\n    mod inner;\n}\n",
            ),
            (
                "src/platform/unix_impl.rs",
                "mod helper;\nuse crate::sys::helper::H;\n",
            ),
            ("src/platform/helper.rs", ""),
            ("testdata/fixtures.rs", ""),
            ("src/outer/inner_impl.rs", "use super::super::sys;\n"),
        ],
    );
    assert_eq!(
        imports(&idx, "src/lib.rs"),
        vec![
            "src/outer/inner_impl.rs",
            "src/platform/unix_impl.rs",
            "testdata/fixtures.rs",
        ]
    );
    // A `#[path]` module owns its own directory for nested `mod` declarations,
    // and `crate::sys` names the module, not its file name.
    assert_eq!(
        imports(&idx, "src/platform/unix_impl.rs"),
        vec!["src/platform/helper.rs"]
    );
    assert_eq!(
        imports(&idx, "src/outer/inner_impl.rs"),
        vec!["src/platform/unix_impl.rs"]
    );
}

#[test]
fn rust_bins_in_a_lib_crate_see_the_lib_by_package_name() {
    let idx = index(
        "rust",
        &[
            ("Cargo.toml", "[package]\nname = \"my-tool\"\n"),
            ("src/lib.rs", "pub mod cfg;\n"),
            ("src/cfg.rs", ""),
            ("src/main.rs", "use my_tool::cfg::Config;\n"),
            ("src/bin/extra/main.rs", "mod args;\nuse my_tool::cfg;\n"),
            ("src/bin/extra/args.rs", "use super::Opts;\n"),
        ],
    );
    assert_eq!(imports(&idx, "src/main.rs"), vec!["src/cfg.rs"]);
    assert_eq!(
        imports(&idx, "src/bin/extra/main.rs"),
        vec!["src/bin/extra/args.rs", "src/cfg.rs"]
    );
    assert_eq!(
        imports(&idx, "src/bin/extra/args.rs"),
        vec!["src/bin/extra/main.rs"]
    );
}

// ------------------------------------------------------------------ typescript

#[test]
fn typescript_relative_module_augmentation_is_an_edge() {
    let idx = index(
        "typescript",
        &[
            ("src/index.ts", "export interface ContextVariableMap {}\n"),
            (
                "src/middleware/jwt/index.ts",
                "import type { X } from 'jose';\n\
                 declare module '../..' {\n  interface ContextVariableMap { jwt: X }\n}\n\
                 declare module 'some-package' {\n  interface Y {}\n}\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "src/middleware/jwt/index.ts"),
        vec!["src/index.ts"]
    );
}

#[test]
fn typescript_tsconfig_paths_and_base_url_resolve_aliases() {
    let idx = index(
        "typescript",
        &[
            (
                "tsconfig.base.json",
                "{\n  // shared options\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \
                 \"paths\": { \"@lib/*\": [\"packages/lib/src/*\"], \"#config\": [\"config/index.ts\"], },\n  },\n}\n",
            ),
            (
                "apps/web/tsconfig.json",
                "{ \"extends\": \"../../tsconfig.base.json\", /* local */ \"include\": [\"src\"] }",
            ),
            (
                "apps/api/tsconfig.json",
                "{ \"extends\": \"../../tsconfig.base.json\",\n  \"compilerOptions\": { \"paths\": { \"~/*\": [\"apps/api/src/*\"] } } }",
            ),
            ("packages/lib/src/money.ts", ""),
            ("packages/lib/src/fmt/index.ts", ""),
            ("config/index.ts", ""),
            ("utils/time.ts", ""),
            (
                "apps/web/src/page.ts",
                "import { m } from '@lib/money';\nimport f from '@lib/fmt';\n\
                 import c from '#config';\nimport t from 'utils/time';\nimport r from 'react';\n",
            ),
            ("apps/api/src/db.ts", ""),
            (
                "apps/api/src/server.ts",
                "import { db } from '~/db';\nimport { m } from '@lib/money';\nimport t from 'utils/time';\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "apps/web/src/page.ts"),
        vec![
            "config/index.ts",
            "packages/lib/src/fmt/index.ts",
            "packages/lib/src/money.ts",
            "utils/time.ts",
        ]
    );
    // The child's `paths` replaces the inherited one; with an inherited
    // `baseUrl` its targets are relative to that `baseUrl`, which also still
    // resolves bare specifiers.
    assert_eq!(
        imports(&idx, "apps/api/src/server.ts"),
        vec!["apps/api/src/db.ts", "utils/time.ts"]
    );
}

#[test]
fn typescript_paths_without_base_url_are_relative_to_their_tsconfig() {
    let idx = index(
        "typescript",
        &[
            (
                "tsconfig.json",
                "{ \"compilerOptions\": { \"paths\": { \"@/*\": [\"./src/*\"] } } }",
            ),
            ("src/a.ts", "import b from '@/b';\nimport c from 'b';\n"),
            ("src/b.ts", ""),
            ("b.ts", ""),
        ],
    );
    // No `baseUrl`: a bare `b` is a package, never `./b.ts`.
    assert_eq!(imports(&idx, "src/a.ts"), vec!["src/b.ts"]);
}

#[test]
fn rust_extern_crate_declarations_depend_on_the_crate_root() {
    let idx = index(
        "rust",
        &[
            (
                "grep/Cargo.toml",
                "[package]\nname = \"grep\"\n[dependencies]\ngrep-cli = { path = \"../cli\" }\n",
            ),
            (
                "grep/src/lib.rs",
                "pub extern crate grep_cli as cli;\nextern crate std;\n",
            ),
            ("cli/Cargo.toml", "[package]\nname = \"grep-cli\"\n"),
            ("cli/src/lib.rs", ""),
        ],
    );
    assert_eq!(imports(&idx, "grep/src/lib.rs"), vec!["cli/src/lib.rs"]);
}

#[test]
fn rust_extern_crate_aliases_resolve_later_use_paths() {
    let idx = index(
        "rust",
        &[
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\n[dependencies]\nfoo = { path = \"../foo\" }\n",
            ),
            (
                "app/src/lib.rs",
                "extern crate foo as bar;\nmod sub;\nuse bar::m::X;\n",
            ),
            // The alias is in the extern prelude of the whole crate.
            ("app/src/sub.rs", "use bar::m::Y;\nuse ::bar::n;\n"),
            ("foo/Cargo.toml", "[package]\nname = \"foo\"\n"),
            ("foo/src/lib.rs", "pub mod m;\npub mod n;\n"),
            ("foo/src/m.rs", "pub struct X;\npub struct Y;\n"),
            ("foo/src/n.rs", ""),
        ],
    );
    assert_eq!(
        imports(&idx, "app/src/lib.rs"),
        vec!["app/src/sub.rs", "foo/src/lib.rs", "foo/src/m.rs"]
    );
    assert_eq!(
        imports(&idx, "app/src/sub.rs"),
        vec!["foo/src/m.rs", "foo/src/n.rs"]
    );
}

// ------------------------------------------------------------------ csharp

#[test]
fn csharp_global_usings_are_scoped_to_their_project() {
    let idx = index(
        "csharp",
        &[
            ("Lib/Lib.csproj", "<Project />"),
            (
                "Lib/Usings.cs",
                "global using Shop.Money;
",
            ),
            (
                "Lib/Cart.cs",
                "namespace Shop.Cart;
class Cart { Price p; }
",
            ),
            ("Money/Money.csproj", "<Project />"),
            (
                "Money/Price.cs",
                "namespace Shop.Money;
public class Price { }
",
            ),
            ("App/App.csproj", "<Project />"),
            (
                "App/Report.cs",
                "namespace Shop.App;
class Report { Price p; }
",
            ),
        ],
    );
    assert_eq!(imports(&idx, "Lib/Cart.cs"), vec!["Money/Price.cs"]);
    assert!(
        imports(&idx, "App/Report.cs").is_empty(),
        "Lib's `global using` does not apply to the App project"
    );
}

#[test]
fn csharp_global_usings_are_repo_wide_without_a_csproj() {
    let idx = index(
        "csharp",
        &[
            (
                "Usings.cs",
                "global using Shop.Money;
",
            ),
            (
                "src/Price.cs",
                "namespace Shop.Money;
public class Price { }
",
            ),
            (
                "src/deep/Cart.cs",
                "namespace Shop.Cart;
class Cart { Price p; }
",
            ),
        ],
    );
    assert_eq!(imports(&idx, "src/deep/Cart.cs"), vec!["src/Price.cs"]);
}

#[test]
fn csharp_inner_scopes_win_over_outer_scopes_and_usings() {
    let idx = index(
        "csharp",
        &[
            (
                "Other.cs",
                "namespace Other { public class Item { } public class Tag { } }
",
            ),
            (
                "Shop/Item.cs",
                "namespace Shop { public class Item { } }
",
            ),
            (
                "Shop/Orders/Item.cs",
                "namespace Shop.Orders { public class Item { } }
",
            ),
            (
                "Shop/Orders/Box.cs",
                "using Other;
namespace Shop.Orders {
                 class Box<Tag> { Item i; Tag t; Inner n; class Inner { } }
}
",
            ),
            (
                "Shop/Orders/Inner.cs",
                "namespace Shop.Orders { public class Inner { } }
",
            ),
            (
                "Shop/Use.cs",
                "using Other;
namespace Shop { class Use { Item i; Tag t; } }
",
            ),
        ],
    );
    assert!(
        imports(&idx, "Shop/Orders/Box.cs") == vec!["Shop/Orders/Item.cs"],
        "the innermost namespace wins over Shop and `using Other`, the nested `Inner` wins          over Shop.Orders.Inner, and the type parameter `Tag` is no type: got {:?}",
        imports(&idx, "Shop/Orders/Box.cs")
    );
    assert_eq!(
        imports(&idx, "Shop/Use.cs"),
        vec!["Other.cs", "Shop/Item.cs"],
        "Shop.Item wins over `using Other`; Tag comes from the using"
    );
}

#[test]
fn csharp_usings_inside_a_namespace_block_apply_only_there() {
    let idx = index(
        "csharp",
        &[
            (
                "Lib.cs",
                "namespace Lib { public class Thing { } }
",
            ),
            (
                "Two.cs",
                "namespace A { using Lib; class X { Thing t; } }
                 namespace B { class Y { Thing t; } }
",
            ),
            (
                "Alias.cs",
                "using L = Lib;
namespace C { class Z { L.Thing t; } }
",
            ),
        ],
    );
    assert_eq!(imports(&idx, "Two.cs"), vec!["Lib.cs"]);
    assert!(
        imports(&idx, "Alias.cs") == vec!["Lib.cs"],
        "an alias of a namespace qualifies names"
    );
    let idx_b_only = index(
        "csharp",
        &[
            (
                "Lib.cs",
                "namespace Lib { public class Thing { } }
",
            ),
            (
                "Two.cs",
                "namespace A { using Lib; }
namespace B { class Y { Thing t; } }
",
            ),
        ],
    );
    assert!(
        imports(&idx_b_only, "Two.cs").is_empty(),
        "`using Lib` inside `namespace A` does not reach `namespace B`"
    );
}

#[test]
fn csharp_ambiguous_names_reach_every_candidate() {
    let idx = index(
        "csharp",
        &[
            (
                "A/Item.cs",
                "namespace A { public class Item { } }
",
            ),
            (
                "B/Item.cs",
                "namespace B { public class Item { } }
",
            ),
            (
                "One/Shared.cs",
                "namespace Shared { public class Dup { } }
",
            ),
            (
                "Two/Shared.cs",
                "namespace Shared { public class Dup { } }
",
            ),
            (
                "Attr/Mark.cs",
                "namespace Attr { public class Mark { } }
",
            ),
            (
                "Attr/MarkAttribute.cs",
                "namespace Attr { public class MarkAttribute : System.Attribute { } }
",
            ),
            (
                "Use.cs",
                "using A;
using B;
using Attr;
                 [Mark] class Use { Item i; Shared.Dup d; }
",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "Use.cs"),
        vec![
            "A/Item.cs",
            "Attr/Mark.cs",
            "Attr/MarkAttribute.cs",
            "B/Item.cs",
            "One/Shared.cs",
            "Two/Shared.cs",
        ],
        "an ambiguous name (two usings, one name declared twice, `[Mark]` with both          Mark and MarkAttribute) reaches every candidate"
    );
}

#[test]
fn csharp_static_member_access_through_a_qualified_name() {
    let idx = index(
        "csharp",
        &[
            (
                "Consts.cs",
                "namespace Shop.Config { public static class Limits { } }
",
            ),
            (
                "Use.cs",
                "class Use { int m = Shop.Config.Limits.Max; int n = local.Limits; }
",
            ),
        ],
    );
    assert_eq!(imports(&idx, "Use.cs"), vec!["Consts.cs"]);
}

#[test]
fn csharp_usings_inside_a_namespace_resolve_relative_to_it() {
    let idx = index(
        "csharp",
        &[
            ("A.csproj", "<Project />"),
            (
                "Order.cs",
                "namespace Shop.Orders { public class Order { } }\n",
            ),
            (
                "M.cs",
                "namespace Shop.Util { public static class M { public class Inner { } } }\n",
            ),
            (
                "Line.cs",
                "namespace Contoso.Domain.Orders { public class Line { } }\n",
            ),
            (
                "Relative.cs",
                "namespace Shop { using Orders; class C { Order o; } }\n",
            ),
            (
                "Static.cs",
                "namespace Shop { using static Util.M; class C { Inner i; } }\n",
            ),
            (
                "Alias.cs",
                "namespace Shop { using O = Orders.Order; class C { O o; } }\n",
            ),
            (
                "FileScoped.cs",
                "namespace Contoso.Api;\nusing Domain.Orders;\nclass C { Line l; }\n",
            ),
        ],
    );
    assert_eq!(imports(&idx, "Relative.cs"), vec!["Order.cs"]);
    assert_eq!(imports(&idx, "Static.cs"), vec!["M.cs"]);
    assert_eq!(imports(&idx, "Alias.cs"), vec!["Order.cs"]);
    assert_eq!(
        imports(&idx, "FileScoped.cs"),
        vec!["Line.cs"],
        "`using Domain.Orders;` after `namespace Contoso.Api;` names Contoso.Domain.Orders"
    );
}

#[test]
fn csharp_top_level_statements_declare_program() {
    let idx = index(
        "csharp",
        &[
            (
                "Api/Api.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk.Web\" />",
            ),
            (
                "Api/Program.cs",
                "var app = 1;\nSystem.Console.WriteLine(app);\n",
            ),
            (
                "Api/Program.Testing.cs",
                "public partial class Program { }\n",
            ),
            (
                "Tests/Tests.csproj",
                "<Project><ItemGroup>\
                 <ProjectReference Include=\"..\\Api\\Api.csproj\" />\
                 </ItemGroup></Project>",
            ),
            (
                "Tests/ApiTests.cs",
                "class ApiTests { Factory<Program> f; object t = typeof(Program); }\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "Tests/ApiTests.cs"),
        vec!["Api/Program.Testing.cs", "Api/Program.cs"],
        "the compiler synthesizes `Program` in the file holding top-level statements"
    );
}

#[test]
fn csharp_implicit_usings_follow_the_project_sdk() {
    let idx = index(
        "csharp",
        &[
            (
                "Directory.Build.props",
                "<Project><PropertyGroup><ImplicitUsings>enable</ImplicitUsings></PropertyGroup></Project>",
            ),
            (
                "Shared/Seeder.cs",
                "namespace Microsoft.AspNetCore.Hosting { public interface IDbSeeder { } }\n",
            ),
            (
                "Bus/Builder.cs",
                "namespace Microsoft.Extensions.DependencyInjection { public interface IBusBuilder { } }\n",
            ),
            (
                "Util/Clock.cs",
                "namespace Shop.Util { public class Clock { } }\n",
            ),
            (
                "Web/Web.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk.Web\" />",
            ),
            ("Web/Use.cs", "class Use { IDbSeeder s; IBusBuilder b; }\n"),
            (
                "Worker/Worker.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk.Worker\"><ItemGroup>\
                 <Using Include=\"Shop.Util\" /></ItemGroup></Project>",
            ),
            (
                "Worker/Use.cs",
                "class Use { IDbSeeder s; IBusBuilder b; Clock c; }\n",
            ),
            (
                "Off/Off.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk.Web\"><PropertyGroup>\
                 <ImplicitUsings>disable</ImplicitUsings></PropertyGroup></Project>",
            ),
            ("Off/Use.cs", "class Use { IDbSeeder s; }\n"),
        ],
    );
    assert_eq!(
        imports(&idx, "Web/Use.cs"),
        vec!["Bus/Builder.cs", "Shared/Seeder.cs"],
        "the Web SDK's implicit usings include Microsoft.AspNetCore.Hosting and \
         Microsoft.Extensions.DependencyInjection (ImplicitUsings from Directory.Build.props)"
    );
    assert_eq!(
        imports(&idx, "Worker/Use.cs"),
        vec!["Bus/Builder.cs", "Util/Clock.cs"],
        "the Worker SDK has no ASP.NET Core usings; a `<Using>` item is a global using"
    );
    assert!(
        imports(&idx, "Off/Use.cs").is_empty(),
        "`ImplicitUsings` disabled in the project wins over Directory.Build.props"
    );
}

#[test]
fn csharp_project_references_scope_the_symbol_table() {
    let idx = index(
        "csharp",
        &[
            (
                "Catalog/Catalog.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk.Web\" />",
            ),
            ("Catalog/Program.cs", "public partial class Program { }\n"),
            (
                "Ordering/Ordering.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk.Web\" />",
            ),
            ("Ordering/Program.cs", "public partial class Program { }\n"),
            ("Ordering/Extensions.cs", "static class Extensions { }\n"),
            (
                "Host/Host.csproj",
                "<Project><ItemGroup>\
                 <Compile Include=\"..\\Shared\\Activity.cs\" Link=\"Activity.cs\" />\
                 <Using Include=\"Host\" />\
                 </ItemGroup></Project>",
            ),
            (
                "Host/Extensions.cs",
                "namespace Host { static class Extensions { } }\n",
            ),
            (
                "Host/AppHost.cs",
                "using Host;\nvar on = Extensions.Enabled;\nvar a = ActivityExtensions.Start;\n",
            ),
            (
                "Shared/Activity.cs",
                "static class ActivityExtensions { Extensions e; }\n",
            ),
            (
                "Tests/Tests.csproj",
                "<Project><ItemGroup>\
                 <ProjectReference Include=\"../Catalog/Catalog.csproj\" />\
                 </ItemGroup></Project>",
            ),
            (
                "Tests/CatalogTests.cs",
                "class CatalogTests { Factory<Program> f; }\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "Tests/CatalogTests.cs"),
        vec!["Catalog/Program.cs"],
        "Ordering's `Program` is not in the Tests compilation"
    );
    assert_eq!(
        imports(&idx, "Host/AppHost.cs"),
        vec!["Host/Extensions.cs", "Shared/Activity.cs"],
        "Ordering's global `Extensions` is not visible, so `using Host;` provides it; \
         a `<Compile Include>` file is part of the project"
    );
    assert_eq!(
        imports(&idx, "Shared/Activity.cs"),
        vec!["Host/Extensions.cs"],
        "a linked file is compiled in the linking project, with its global usings"
    );
}

#[test]
fn csharp_a_using_alias_shadows_using_namespace_imports() {
    let idx = index(
        "csharp",
        &[
            (
                "Domain/Order.cs",
                "namespace Shop.Domain { public class Order { } }\n",
            ),
            (
                "Queries/Order.cs",
                "namespace Shop.Queries { public record Order; }\n",
            ),
            ("Usings.cs", "global using Shop.Domain;\n"),
            (
                "Api.cs",
                "using Order = Shop.Queries.Order;\nclass Api { Order o; }\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "Api.cs"),
        vec!["Queries/Order.cs"],
        "C# binds `Order` to the alias, not to the global using's Shop.Domain.Order"
    );
}

// ------------------------------------------------------------------ php

const PSR4_APP: &str = r#"{"autoload": {"psr-4": {"App\\": "app/"}}}"#;

#[test]
fn php_names_resolve_by_php_rules_through_psr4() {
    let idx = index(
        "php",
        &[
            ("composer.json", PSR4_APP),
            (
                "app/Models/User.php",
                "<?php\nnamespace App\\Models;\nclass User {}\n",
            ),
            (
                "app/Models/Post.php",
                "<?php\nnamespace App\\Models;\nclass Post {}\n",
            ),
            (
                "app/Models/Tag.php",
                "<?php\nnamespace App\\Models;\nclass Tag {}\n",
            ),
            (
                "app/Http/Base.php",
                "<?php\nnamespace App\\Http;\nclass Base {}\n",
            ),
            (
                "app/Http/Sub/Inner.php",
                "<?php\nnamespace App\\Http\\Sub;\nclass Inner {}\n",
            ),
            (
                "app/Http/Rel.php",
                "<?php\nnamespace App\\Http;\nclass Rel {}\n",
            ),
            (
                "app/Support/Fqn.php",
                "<?php\nnamespace App\\Support;\nclass Fqn {}\n",
            ),
            ("app/Global.php", "<?php\nclass Exception {}\n"),
            (
                "app/Http/Controller.php",
                "<?php\nnamespace App\\Http;\n\
                 use App\\Models\\User;\n\
                 use App\\Models as M;\n\
                 class Controller extends Base {\n\
                   function a(User $u): M\\Post { return new Sub\\Inner(); }\n\
                   function b() { \\App\\Support\\Fqn::make(); namespace\\Rel::x(); }\n\
                   function c() { throw new Exception(); }\n\
                 }\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "app/Http/Controller.php"),
        vec![
            "app/Http/Base.php",
            "app/Http/Rel.php",
            "app/Http/Sub/Inner.php",
            "app/Models/Post.php",
            "app/Models/User.php",
            "app/Support/Fqn.php",
        ],
        "unqualified names go through `use` or the current namespace, qualified names through \
         a use alias or the current namespace, `\\X` is absolute; `Exception` in namespace \
         App\\Http is App\\Http\\Exception, never the global class (no fallback for classes)"
    );
    assert!(imports(&idx, "app/Models/Tag.php").is_empty());
}

#[test]
fn php_psr4_tries_longest_prefix_first_and_every_directory_of_a_prefix() {
    let idx = index(
        "php",
        &[
            (
                "composer.json",
                r#"{"autoload": {"psr-4": {"App\\": ["src/", "lib/"], "App\\Core\\": "core/"}},
                    "autoload-dev": {"psr-4": {"Tests\\": "tests/"}}}"#,
            ),
            ("src/A.php", "<?php\nnamespace App;\nclass A {}\n"),
            ("lib/B.php", "<?php\nnamespace App;\nclass B {}\n"),
            (
                "core/Kernel.php",
                "<?php\nnamespace App\\Core;\nclass Kernel {}\n",
            ),
            (
                "tests/Helper.php",
                "<?php\nnamespace Tests;\nclass Helper {}\n",
            ),
            (
                "tests/ATest.php",
                "<?php\nnamespace Tests;\nuse App\\{A, B};\nuse App\\Core\\Kernel;\n\
                 class ATest { function t(A $a, B $b, Kernel $k, Helper $h) {} }\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "tests/ATest.php"),
        vec![
            "core/Kernel.php",
            "lib/B.php",
            "src/A.php",
            "tests/Helper.php"
        ],
        "App\\ maps to src/ then lib/; App\\Core\\ (longer) maps to core/; autoload-dev counts"
    );
}

#[test]
fn php_psr4_prefers_the_mapped_file_over_other_declarations() {
    let idx = index(
        "php",
        &[
            ("composer.json", PSR4_APP),
            ("app/Mailer.php", "<?php\nnamespace App;\nclass Mailer {}\n"),
            (
                "legacy/Mailer.php",
                "<?php\nnamespace App;\nclass Mailer {}\n",
            ),
            (
                "app/Use.php",
                "<?php\nnamespace App;\nclass UseIt { function m(Mailer $m) {} }\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "app/Use.php"),
        vec!["app/Mailer.php"],
        "the autoloader loads the PSR-4 file; declarations elsewhere are only a fallback"
    );
}

#[test]
fn php_psr0_maps_namespaces_and_underscores_to_directories() {
    let idx = index(
        "php",
        &[
            (
                "composer.json",
                r#"{"autoload": {"psr-0": {"Legacy_": "lib/", "Vendor\\Pkg\\": "src/"}}}"#,
            ),
            (
                "lib/Legacy/Db/Table.php",
                "<?php\nclass Legacy_Db_Table {}\n",
            ),
            (
                "src/Vendor/Pkg/Thing/Part.php",
                "<?php\nnamespace Vendor\\Pkg;\nclass Thing_Part {}\n",
            ),
            (
                "index.php",
                "<?php\nnew Legacy_Db_Table();\nnew \\Vendor\\Pkg\\Thing_Part();\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "index.php"),
        vec!["lib/Legacy/Db/Table.php", "src/Vendor/Pkg/Thing/Part.php"],
        "PSR-0 keeps the prefix in the path and turns `_` in the class name into `/`"
    );
}

#[test]
fn php_nested_composer_projects_map_relative_to_their_own_directory() {
    let idx = index(
        "php",
        &[
            ("composer.json", PSR4_APP),
            (
                "packages/billing/composer.json",
                r#"{"autoload": {"psr-4": {"Billing\\": "src/"}}}"#,
            ),
            (
                "packages/billing/src/Invoice.php",
                "<?php\nnamespace Billing;\nclass Invoice {}\n",
            ),
            (
                "app/Order.php",
                "<?php\nnamespace App;\nuse Billing\\Invoice;\nclass Order { public Invoice $i; }\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "app/Order.php"),
        vec!["packages/billing/src/Invoice.php"]
    );
}

#[test]
fn php_falls_back_to_declared_classes_case_insensitively() {
    let idx = index(
        "php",
        &[
            (
                "legacy/helpers.php",
                "<?php\nclass LegacyHelper {}\ninterface Shape {}\n",
            ),
            (
                "legacy/more/Traits.php",
                "<?php\nnamespace Lib;\ntrait Loggable {}\nenum Color {}\n",
            ),
            (
                "web/page.php",
                "<?php\nclass Page implements shape { use \\Lib\\Loggable; }\n\
                 legacyhelper::run();\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "web/page.php"),
        vec!["legacy/helpers.php", "legacy/more/Traits.php"],
        "classes outside any autoload map are found by declaration; class names are \
         case-insensitive"
    );
}

#[test]
fn php_use_function_and_const_resolve_only_to_declaring_files() {
    let idx = index(
        "php",
        &[
            (
                "src/functions.php",
                "<?php\nnamespace App\\Support;\nfunction helper() {}\nconst LIMIT = 1;\n",
            ),
            (
                "src/Klass.php",
                "<?php\nnamespace App\\Support;\nclass Klass { const LIMIT = 2; }\n",
            ),
            (
                "src/use.php",
                "<?php\nuse function App\\Support\\helper;\nuse const App\\Support\\LIMIT;\n\
                 use function App\\Support\\missing;\nuse function strlen;\n",
            ),
        ],
    );
    assert_eq!(imports(&idx, "src/use.php"), vec!["src/functions.php"]);
}

#[test]
fn php_includes_resolve_relative_to_the_including_file() {
    let idx = index(
        "php",
        &[
            ("bootstrap/app.php", "<?php\n"),
            ("public/lib/x.php", "<?php\n"),
            ("public/partials/head.phtml", "<p>head</p>\n"),
            ("config/db.php", "<?php\nreturn [];\n"),
            (
                "public/index.php",
                "<?php\nrequire __DIR__ . '/../bootstrap/app.php';\nrequire_once 'lib/x.php';\n\
                 include 'partials/head.phtml';\n$c = require dirname(__DIR__) . '/config/db.php';\n\
                 include '../../outside.php';\nrequire 'missing.php';\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "public/index.php"),
        vec![
            "bootstrap/app.php",
            "config/db.php",
            "public/lib/x.php",
            "public/partials/head.phtml",
        ],
        "a literal or `__DIR__`-based include names a repository file; an escaping or missing \
         path is dropped"
    );
}

#[test]
fn php_vendor_is_third_party() {
    let idx = index(
        "php",
        &[
            ("composer.json", PSR4_APP),
            ("vendor/autoload.php", "<?php\n"),
            (
                "vendor/acme/lib/composer.json",
                r#"{"autoload": {"psr-4": {"App\\": "../../../app2/"}}}"#,
            ),
            (
                "vendor/acme/lib/src/Client.php",
                "<?php\nnamespace Acme;\nclass Client {}\n",
            ),
            (
                "vendor/laravel/framework/src/Model.php",
                "<?php\nnamespace Illuminate\\Database;\nclass Model {}\n",
            ),
            // Reachable only through the vendored composer.json: it declares nothing.
            ("app2/Shadow.php", "<?php\n"),
            (
                "app/User.php",
                "<?php\nnamespace App;\nrequire __DIR__ . '/../vendor/autoload.php';\n\
                 use Illuminate\\Database\\Model;\nuse Acme\\Client;\n\
                 class User extends Model { function c(Client $c, Shadow $s) {} }\n",
            ),
        ],
    );
    assert!(
        imports(&idx, "app/User.php").is_empty(),
        "vendor/ files are never targets, and composer.json files under vendor/ add no \
         autoload roots; got {:?}",
        imports(&idx, "app/User.php")
    );
}

#[test]
fn php_use_imports_apply_only_in_their_namespace() {
    let idx = index(
        "php",
        &[
            ("A/Item.php", "<?php\nnamespace A;\nclass Item {}\n"),
            ("B/Item.php", "<?php\nnamespace B;\nclass Item {}\n"),
            (
                "two.php",
                "<?php\nnamespace X;\nuse A\\Item;\nnew Item();\nnamespace Y;\nnew Item();\n",
            ),
        ],
    );
    assert_eq!(
        imports(&idx, "two.php"),
        vec!["A/Item.php"],
        "`use A\\Item` applies in namespace X only; `Item` in Y is Y\\Item, not declared"
    );
}
