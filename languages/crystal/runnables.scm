(
  (call
    method: (identifier) @run
    (#match? @run "^(describe|context|it|pending)$")
  )
  (#set! tag crystal-spec)
)