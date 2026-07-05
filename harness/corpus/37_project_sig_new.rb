# ADR-0033: a class DECLARED in the project's own `sig/` (see the sibling
# `37_project_sig_new.sig/`) is authoritative — the reference (and rigor-rs)
# witness `call.undefined-method` on an `X.new` instance-method typo of such a
# class, exactly as it does for a core class. Byte-for-byte against the oracle
# on (rule, line, column = the method token).

# `Widget` is declared in sig/ with `def spin: () -> Integer`.
w = Widget.new

# `spin` is declared ⇒ resolves ⇒ NO fire.
w.spin

# `spni` is a typo ⇒ witnessed absent over Widget's (sig + Object) chain ⇒ FIRE.
w.spni

# The direct-chain form fires too (`.new` instance typed without a binding).
Widget.new.spni
