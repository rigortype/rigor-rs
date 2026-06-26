# Expected reference diagnostics: (none for call.undefined-method)
#
# rigor-rs status: SUPPORTED — ADR-0023 tier-4b call-site PARAMETER BINDING
# decline guard. `echo` takes a SPLAT (`*xs`), which breaks the positional
# index<->arg alignment, so param binding is declined entirely: `echo("a")`
# types Dynamic and the chained `.lenght` stays silent. The reference is ALSO
# silent on the splat case (no witness through the splat method here), so this
# is a clean zero-FP match.
class Decliner
  def echo(*xs)
    xs
  end
end
d = Decliner.new
d.echo("a").lenght
