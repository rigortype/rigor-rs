# ADR-0033 negatives: the project-sig witnessing must NOT over-fire. All calls
# here are legitimate or fall under the reference's leniency, so the expected
# diagnostic set is EMPTY.

# `Widget` is declared in sig/ with `spin` + `describe` (see the sibling
# `38_project_sig_negatives.sig/`).
w = Widget.new

# Declared instance methods ⇒ resolve ⇒ NO fire.
w.spin
w.describe

# An Object-inherited method resolves over Widget's chain ⇒ NO fire.
w.to_s
w.frozen?

# A bundled stdlib class (`Pathname`, NOT project-sig) keeps the reference's
# `.new` leniency: a typo on its instance is NOT witnessed ⇒ NO fire.
Pathname.new("x").nope
