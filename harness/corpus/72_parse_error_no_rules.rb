# A file Prism cannot parse gets NO semantic diagnostics.
#
# The reference's `analyze_file_body` returns early on
# `parse_result.errors.any?` — it emits the parse errors and never reaches
# `ScopeIndexer.index`, so every semantic rule is off for the whole file.
# rigor-rs used to lower Prism's RECOVERED tree and run the rules over it, and
# recovery invents bindings: the header below recovers into a body that
# references an `element` nobody ever bound, which the toplevel-call rule then
# reported four times (rigor-survey `Ruby/searches/fibonacci_search.rb`).
#
# The reference's parse diagnostics (`rule: null`) are a coverage gap here —
# rigor-rs emits none — which the harness allows. What it must NOT do is emit a
# rule diagnostic the reference does not.
def broken_header int arr, int element
  n = arr.size

  while f < n do
    f = element
  end

  # An outright undefined method on a known literal: the rule that would fire on
  # a well-formed file, proving the whole file is off and not just its header.
  "hello".lenght

  totally_undefined_toplevel_call
end
