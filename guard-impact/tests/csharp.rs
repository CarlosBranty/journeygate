//! C# plugin against `tests/fixtures/csharp/` (two projects, Shop.Domain and
//! Shop.App, that `dotnet build` compiles). An edge means "references a type
//! declared in that file".
mod common;

use common::{index, sorted, strings};
use guard_core::ports::ImpactAnalyzer;
use guard_impact::Language;

const CURRENCY: &str = "Shop.Domain/Billing/Currency.cs";
const MONEY: &str = "Shop.Domain/Billing/Money.cs";
const AUDITED: &str = "Shop.Domain/Auditing/AuditedAttribute.cs";
const LINES: &str = "Shop.Domain/Orders/Order.Lines.cs";
const TOTALS: &str = "Shop.Domain/Orders/Order.Totals.cs";
const REPOSITORY: &str = "Shop.Domain/Orders/IOrderRepository.cs";
const EXCEPTION: &str = "Shop.Domain/Orders/OrderException.cs";
const PRICING: &str = "Shop.Domain/Orders/Pricing.cs";
const REPORT: &str = "Shop.App/Reports/Report.cs";

#[test]
fn cs_files_use_the_csharp_plugin() {
    let idx = index("csharp");
    assert_eq!(idx.language_of(MONEY), Some(Language::CSharp));
    assert_eq!(idx.language_of("Shop.App/Shop.App.csproj"), None);
}

#[test]
fn same_namespace_types_resolve_in_file_scoped_namespaces() {
    let idx = index("csharp");
    assert_eq!(
        idx.imports_of(MONEY),
        vec![CURRENCY],
        "`Currency` is declared in Money.cs's own file-scoped namespace"
    );
    assert!(idx.imports_of(CURRENCY).is_empty());
}

#[test]
fn global_usings_apply_to_every_file_of_their_project() {
    let idx = index("csharp");
    assert_eq!(
        idx.imports_of(LINES),
        vec![MONEY],
        "`Money` comes from Shop.Domain's `global using Shop.Domain.Billing`"
    );
}

#[test]
fn nested_types_of_the_enclosing_type_resolve_to_every_partial_part() {
    let idx = index("csharp");
    assert_eq!(
        sorted(idx.imports_of(TOTALS)),
        vec![CURRENCY, MONEY, LINES],
        "`Line` is nested in the enclosing partial `Order`, declared in Order.Lines.cs"
    );
    assert_eq!(
        sorted(idx.imports_of(REPORT)),
        vec![CURRENCY, MONEY, LINES, TOTALS],
        "`Order` is partial: a reference depends on every file declaring a part"
    );
}

#[test]
fn attributes_generic_arguments_and_bcl_names() {
    let idx = index("csharp");
    assert_eq!(
        sorted(idx.imports_of(REPOSITORY)),
        vec![AUDITED, LINES, TOTALS],
        "`[Audited]` is AuditedAttribute, `Order` a generic argument; `Task` is \
         System.Threading.Tasks.Task, never Shop.Domain.Scheduling.Task"
    );
    assert!(
        idx.importers_of("Shop.Domain/Scheduling/Task.cs")
            .is_empty()
    );
    assert!(
        idx.imports_of(AUDITED).is_empty(),
        "AttributeUsage, AttributeTargets and Attribute are BCL types"
    );
    assert_eq!(
        sorted(idx.imports_of(PRICING)),
        vec![AUDITED, CURRENCY],
        "`[Auditing.Audited]` resolves through the parent namespace Shop.Domain"
    );
}

#[test]
fn aliases_static_usings_and_expression_positions() {
    let idx = index("csharp");
    assert_eq!(
        sorted(idx.imports_of("Shop.App/Program.cs")),
        vec![CURRENCY, REPOSITORY, LINES, EXCEPTION, PRICING],
        "alias target, `using static`, static member access through Shop.App's \
         global using, `typeof(Order.Line)` and a catch type"
    );
    assert_eq!(
        sorted(idx.imports_of("Shop.App/Reports/Formatter.cs")),
        vec![REPORT, EXCEPTION],
        "declaration pattern and `as`; the type parameter `T` is no type"
    );
    assert!(idx.imports_of("Shop.App/Usings.cs").is_empty());
    assert!(idx.imports_of("Shop.Domain/GlobalUsings.cs").is_empty());
}

#[test]
fn fan_out_and_transitive_fan_out() {
    let idx = index("csharp");
    let facts = idx.fan_out(&strings(&[CURRENCY])).unwrap();
    assert_eq!(facts.max_fan_out, 5);
    assert_eq!(
        sorted(idx.importers_of(CURRENCY)),
        vec!["Shop.App/Program.cs", REPORT, MONEY, TOTALS, PRICING]
    );
    // + Order.Lines.cs (via Money), IOrderRepository.cs (via Order),
    // Formatter.cs (via Report).
    assert_eq!(idx.transitive_fan_out(CURRENCY), 8);
}
