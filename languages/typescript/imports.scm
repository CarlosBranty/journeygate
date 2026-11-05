; Import extraction for TypeScript (.ts, .mts, .cts) and TSX (.tsx).
; The same query runs against both grammars.
;
; Capture contract (read by guard-impact):
;   @import  the module specifier text (the string contents, without quotes)
; Captures whose name starts with "_" are helpers and are ignored.

; import x from './x'; import type { T } from './t'; import './side-effect';
(import_statement
  source: (string (string_fragment) @import))

; import x = require('./x');
(import_require_clause
  source: (string (string_fragment) @import))

; export { x } from './x'; export * from './x';
(export_statement
  source: (string (string_fragment) @import))

; require('./x')
(call_expression
  function: (identifier) @_fn
  arguments: (arguments . (string (string_fragment) @import))
  (#eq? @_fn "require"))

; import('./x')
(call_expression
  function: (import)
  arguments: (arguments . (string (string_fragment) @import)))

; declare module '../x' { ... }: a module augmentation depends on the module it
; augments. Only relative names: a bare name may be an ambient declaration.
; Query string literals unescape `\\` to `\` (and an unknown escape such as
; `\.` to a bare `.`), so the regex below is ^\.\.?(/|$).
(ambient_declaration
  (module
    name: (string (string_fragment) @import))
  (#match? @import "^\\.\\.?(/|$)"))
