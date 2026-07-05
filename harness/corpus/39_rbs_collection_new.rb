# ADR-0034: a gem whose RBS a project pulled in with `rbs collection install`
# (recorded in the sibling `39_rbs_collection_new.collection/`) is authoritative
# — the reference (and rigor-rs) attribute it to the signature-path tier and
# witness a `.new` instance-method typo on it, exactly as for project sig/.
# Byte-for-byte against the oracle on (rule, line, column = the method token).

# `Mailer` is declared in the collection with `def deliver: () -> bool`.
m = Mailer.new

# `deliver` is declared ⇒ resolves ⇒ NO fire.
m.deliver

# `delvier` is a typo ⇒ witnessed absent over the collection-declared chain
# (Mailer + Object) ⇒ FIRE.
m.delvier
