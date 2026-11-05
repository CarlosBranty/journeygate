; Import extraction for Go (.go).
;
; Capture contract (read by guard-impact):
;   @import  the import path literal, quotes included ("a/b" or `a/b`)

(import_spec
  path: (_) @import)
