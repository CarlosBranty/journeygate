; Import extraction for Rust (.rs).
;
; Capture contract (read by guard-impact):
;   @use  the argument of a `use` declaration (a use tree, expanded by guard-impact)
;   @mod  the name of an out-of-line module declaration (`mod foo;`)
;   @extern  the crate name of `extern crate foo;` / `extern crate foo as bar;`

(use_declaration
  argument: (_) @use)

(mod_item
  name: (identifier) @mod
  !body)

(extern_crate_declaration
  name: (identifier) @extern)
