; Import extraction for JavaScript (.js, .jsx, .mjs, .cjs).
;
; Capture contract (read by guard-impact):
;   @import  the module specifier text (the string contents, without quotes)
; Captures whose name starts with "_" are helpers and are ignored.

; import x from './x'; import './side-effect';
(import_statement
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
