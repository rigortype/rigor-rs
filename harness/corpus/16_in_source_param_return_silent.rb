# Expected reference diagnostics: (none for call.undefined-method)
#
# rigor-rs status: SUPPORTED — ADR-0023 tier-4b zero-FP gate. `name`'s tail is an
# ivar read, which types Dynamic under the EMPTY env, so NO return entry is
# stored and `c.name` types Dynamic — the chained `.lenght` must stay silent.
# The reference is ALSO silent here (no ivar type), so this is a clean match.
class C
  def name
    @name
  end
end
c = C.new
c.name.lenght
