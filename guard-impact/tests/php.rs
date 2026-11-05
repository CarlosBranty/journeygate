//! PHP plugin against `tests/fixtures/php/`, a Laravel-style app: PSR-4
//! `App\` under `app/`, factories and tests through `autoload` /
//! `autoload-dev`, classmap seeders, a `files` helper, static includes from
//! `public/` and `bootstrap/`, and a `vendor/` tree that is third-party.
mod common;

use common::{index, sorted, strings};
use guard_core::ports::ImpactAnalyzer;
use guard_impact::Language;

const USER: &str = "app/Models/User.php";
const POST: &str = "app/Models/Post.php";
const STATUS: &str = "app/Enums/Status.php";
const HAS_ROLES: &str = "app/Models/Concerns/HasRoles.php";
const CONTROLLER: &str = "app/Http/Controllers/Controller.php";
const USER_CONTROLLER: &str = "app/Http/Controllers/UserController.php";
const REPOSITORY: &str = "app/Contracts/UserRepository.php";
const ELOQUENT_REPOSITORY: &str = "app/Repositories/EloquentUserRepository.php";
const HELPERS: &str = "app/Support/helpers.php";
const FACTORY: &str = "database/factories/UserFactory.php";
const SEEDER: &str = "database/seeders/DatabaseSeeder.php";
const VENDOR_MODEL: &str = "vendor/laravel/framework/src/Illuminate/Database/Eloquent/Model.php";

#[test]
fn php_files_use_the_php_plugin() {
    let idx = index("php");
    assert_eq!(idx.language_of(USER), Some(Language::Php));
    assert_eq!(idx.language_of("composer.json"), None);
    assert!(idx.fallback_languages().is_empty());
}

#[test]
fn psr4_use_imports_traits_enums_and_class_constants() {
    let idx = index("php");
    assert_eq!(
        idx.imports_of(USER),
        vec![STATUS, HAS_ROLES, POST, FACTORY],
        "`use` imports, trait `use`, `Status::class` and `Post::class` (same namespace) resolve \
         through PSR-4; Illuminate classes live in vendor/ and are dropped"
    );
    assert_eq!(idx.imports_of(POST), vec![USER]);
}

#[test]
fn every_class_position_of_a_controller_resolves() {
    let idx = index("php");
    assert_eq!(
        idx.imports_of(USER_CONTROLLER),
        vec![
            "app/Attributes/Audited.php",
            REPOSITORY,
            STATUS,
            "app/Exceptions/UserNotFound.php",
            CONTROLLER,
            POST,
            USER,
            HELPERS,
        ],
        "attribute, promoted constructor parameter, catch types, instanceof, enum case access, \
         a grouped `use` with an alias, the same-namespace base class, and `use function` / \
         `use const` of the autoloaded helpers file"
    );
}

#[test]
fn a_namespace_use_alias_qualifies_relative_names() {
    let idx = index("php");
    assert_eq!(
        idx.imports_of(ELOQUENT_REPOSITORY),
        vec![REPOSITORY, USER],
        "`use App\\Models;` makes `Models\\User` mean `App\\Models\\User`"
    );
}

#[test]
fn includes_chain_the_front_controller_to_routes_but_not_into_vendor() {
    let idx = index("php");
    assert_eq!(
        idx.imports_of("public/index.php"),
        vec!["bootstrap/app.php"],
        "`require __DIR__ . '/../vendor/autoload.php'` names third-party code"
    );
    assert_eq!(
        idx.imports_of("bootstrap/app.php"),
        vec![USER_CONTROLLER, ELOQUENT_REPOSITORY, "routes/web.php"]
    );
    assert_eq!(idx.imports_of("routes/web.php"), vec![USER_CONTROLLER]);
}

#[test]
fn classmap_classes_resolve_through_their_declaration() {
    let idx = index("php");
    assert_eq!(
        idx.imports_of("tests/Feature/UserTest.php"),
        vec![USER, SEEDER, "tests/TestCase.php"],
        "`\\DatabaseSeeder` has no namespace and no PSR mapping; the symbol table finds it"
    );
    assert_eq!(idx.imports_of(SEEDER), vec![USER]);
}

#[test]
fn vendor_files_are_never_targets() {
    let idx = index("php");
    assert!(idx.files().any(|f| f == VENDOR_MODEL));
    assert!(idx.importers_of(VENDOR_MODEL).is_empty());
    assert!(idx.importers_of("vendor/autoload.php").is_empty());
    assert!(
        idx.files()
            .filter(|f| !f.starts_with("vendor/"))
            .flat_map(|f| idx.imports_of(f))
            .all(|t| !t.starts_with("vendor/")),
        "no repository file depends on vendor/"
    );
}

#[test]
fn fan_out_counts_direct_importers_and_the_closure_reaches_entry_points() {
    let idx = index("php");
    assert_eq!(
        sorted(idx.importers_of(USER)),
        vec![
            REPOSITORY,
            USER_CONTROLLER,
            POST,
            ELOQUENT_REPOSITORY,
            FACTORY,
            SEEDER,
            "tests/Feature/UserTest.php",
        ]
    );
    let facts = idx.fan_out(&strings(&[USER])).unwrap();
    assert_eq!(facts.fan_out[USER], 7);
    assert_eq!(facts.max_fan_out, 7);
    let closure = idx.transitive_importers(STATUS);
    for entry in ["public/index.php", "bootstrap/app.php", "routes/web.php"] {
        assert!(
            closure.contains(&entry),
            "{entry} reaches Status: {closure:?}"
        );
    }
}

#[test]
fn areas_and_hard_rules_apply_to_php_paths() {
    let idx = index("php");
    let migration = "database/migrations/2024_01_01_000000_create_users_table.php";
    let facts = idx.fan_out(&strings(&[migration])).unwrap();
    assert!(facts.human_only_path_hit && facts.schema_migration);
    let areas = idx.areas_for(&strings(&[USER]));
    assert_eq!(
        areas.iter().map(|a| a.as_str()).collect::<Vec<_>>(),
        vec!["models"]
    );
}
