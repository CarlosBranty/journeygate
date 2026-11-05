; Dependency extraction for PHP (.php).
;
; File A depends on file B when A names a class, interface, trait or enum
; that B declares, imports a function or constant that B declares, or
; statically includes B. guard-impact reads the namespace of every capture
; from its ancestors and siblings, so the patterns below stay flat.
;
; Capture contract (read by guard-impact):
;   @declare   the name of a class / interface / trait / enum declaration
;   @function  the name of a function declaration
;   @const     the name of a `const X = ...;` element (class constants are
;              filtered out by guard-impact)
;   @use       a `use` import declaration (class, function, const, grouped)
;   @class     a name, qualified name or relative name (`namespace\X`) in
;              a class position
;   @include   a require / require_once / include / include_once expression

; ---------------------------------------------------------------- declarations

(class_declaration name: (name) @declare)
(interface_declaration name: (name) @declare)
(trait_declaration name: (name) @declare)
(enum_declaration name: (name) @declare)
(function_definition name: (name) @function)
(const_declaration (const_element (name) @const))

; ---------------------------------------------------------------- imports

(namespace_use_declaration) @use

[
  (require_expression)
  (require_once_expression)
  (include_expression)
  (include_once_expression)
] @include

; ---------------------------------------------------------------- class names

; Type hints of parameters, returns, properties and catch clauses (also
; inside union, intersection and nullable types).
(named_type [(name) (qualified_name) (relative_name)] @class)

; new X, extends, implements, trait use inside a class body.
(object_creation_expression [(name) (qualified_name) (relative_name)] @class)
(base_clause [(name) (qualified_name) (relative_name)] @class)
(class_interface_clause [(name) (qualified_name) (relative_name)] @class)
(use_declaration [(name) (qualified_name) (relative_name)] @class)

; X::call(), X::$prop, X::CONST and X::class.
(scoped_call_expression scope: [(name) (qualified_name) (relative_name)] @class)
(scoped_property_access_expression scope: [(name) (qualified_name) (relative_name)] @class)
(class_constant_access_expression . [(name) (qualified_name) (relative_name)] @class)

; $x instanceof X
(binary_expression operator: "instanceof" right: [(name) (qualified_name) (relative_name)] @class)

; #[X(...)]
(attribute [(name) (qualified_name) (relative_name)] @class)
