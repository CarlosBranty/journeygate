//! Python plugin against `tests/fixtures/python/`.
mod common;

use common::{index, sorted, strings};
use guard_core::ports::{Area, ImpactAnalyzer};
use guard_impact::Language;

#[test]
fn relative_and_absolute_imports_create_edges() {
    let idx = index("python");
    // `from ..models import Order` and `from shop import models` (submodule name).
    assert_eq!(
        sorted(idx.importers_of("shop/models.py")),
        vec!["shop/orders/service.py", "shop/payments/gateway.py"]
    );
    // `from .service import`, `from shop.orders import service`, `from shop.orders.service import`.
    assert_eq!(
        sorted(idx.importers_of("shop/orders/service.py")),
        vec!["app.py", "shop/orders/__init__.py", "tests/test_orders.py"]
    );
    // `import shop.payments.gateway as gw`.
    assert_eq!(
        idx.importers_of("shop/payments/gateway.py"),
        vec!["shop/orders/service.py"]
    );
    // A package import reaches its `__init__.py`.
    assert_eq!(idx.importers_of("shop/orders/__init__.py"), vec!["app.py"]);
    assert_eq!(
        idx.importers_of("shop/__init__.py"),
        vec!["shop/payments/gateway.py"]
    );
}

#[test]
fn third_party_and_stdlib_imports_are_ignored() {
    let idx = index("python");
    assert_eq!(
        idx.imports_of("app.py"),
        vec!["shop/orders/__init__.py", "shop/orders/service.py"]
    );
    assert_eq!(idx.language_of("app.py"), Some(Language::Python));
}

#[test]
fn fan_out_and_transitive_fan_out() {
    let idx = index("python");
    let facts = idx.fan_out(&strings(&["shop/models.py"])).unwrap();
    assert_eq!(facts.max_fan_out, 2);
    assert_eq!(idx.transitive_fan_out("shop/models.py"), 5);
    assert!(!facts.schema_migration);
    assert_eq!(
        idx.areas_for(&strings(&["shop/payments/gateway.py"])),
        vec![Area::new("payments")]
    );
}
