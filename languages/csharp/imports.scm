; Type-reference extraction for C# (.cs).
;
; C# has no file imports: file A depends on file B when A names a type that
; B declares. guard-impact reads the namespace and enclosing types of every
; capture from its ancestors, so the patterns below stay flat.
;
; Capture contract (read by guard-impact):
;   @declare    the name of a class / struct / interface / enum / record /
;               delegate declaration (a partial type has one per part)
;   @program    a top-level statement: its file declares the compiler's
;               global `Program` class
;   @using      a using directive (plain, static, alias, `global`)
;   @type       a node in a type position (identifier, generic_name,
;               qualified_name, alias_qualified_name, array / nullable /
;               pointer / ref type); predefined and `var` types yield nothing
;   @attribute  the name of an attribute (`Foo` also means `FooAttribute`)
;   @expr       an expression that may name a type: the receiver of a member
;               access (`Pricing.Rate()`), a `nameof` argument, a constant
;               pattern (`x is Foo`, `case Color.Red`)

; ---------------------------------------------------------------- declarations

(class_declaration name: (identifier) @declare)
(struct_declaration name: (identifier) @declare)
(interface_declaration name: (identifier) @declare)
(enum_declaration name: (identifier) @declare)
(record_declaration name: (identifier) @declare)
(delegate_declaration name: (identifier) @declare)

; Top-level statements: the compiler synthesizes `Program` in this file.
(compilation_unit (global_statement) @program)

; ---------------------------------------------------------------- usings

(using_directive) @using

; ---------------------------------------------------------------- type positions

; Fields, properties, parameters, locals, casts, object creation, typeof,
; default, catch, declaration / type patterns, foreach, constraints,
; tuple elements, element types of arrays and nullables, delegates.
(_ type: (_) @type)

; Method and function-pointer return types.
(_ returns: (_) @type)

; `x as Foo`, `x is Foo` (when parsed as a type test).
(as_expression right: (_) @type)
(is_expression right: (_) @type)

; Base classes and interfaces.
(base_list (_) @type)

; Generic arguments, also nested (`Dictionary<Currency, List<Money>>`).
(type_argument_list (_) @type)

; Explicit interface implementations (`void IFoo.Bar()`).
(explicit_interface_specifier (_) @type)

; ---------------------------------------------------------------- attributes

(attribute name: (_) @attribute)

; ---------------------------------------------------------------- expressions

; Static member access on a type: `Pricing.Rate(x)`, `Color.Red`,
; `Shop.Config.Limits.Max` (each receiver level is captured).
(member_access_expression expression: (_) @expr)

; `nameof(Foo)`.
(invocation_expression
  function: (identifier) @_fn
  arguments: (argument_list (argument (_) @expr))
  (#eq? @_fn "nameof"))

; `x is Foo` / `case Color.Red:` parsed as a constant pattern.
(constant_pattern (_) @expr)
