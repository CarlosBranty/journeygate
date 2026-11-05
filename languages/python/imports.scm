; Import extraction for Python (.py, .pyi).
;
; Capture contract (read by guard-impact):
;   @import  an absolute dotted module of `import a.b.c` / `import a.b as c`
;   @from    the module of `from <module> import ...` (dotted or relative, e.g. `..pkg`)
;   @name    one imported name of that same `from` statement (may be a submodule)
; A match carries either @import, or @from with an optional @name.

(import_statement
  name: (dotted_name) @import)

(import_statement
  name: (aliased_import
    name: (dotted_name) @import))

; from pkg import a, b as c  (one match per name)
(import_from_statement
  module_name: (_) @from
  name: [
    (dotted_name) @name
    (aliased_import name: (dotted_name) @name)
  ])

; from pkg import *
(import_from_statement
  module_name: (_) @from
  (wildcard_import))
