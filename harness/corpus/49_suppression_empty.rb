# `suppression.empty` fires for a bare `# rigor:disable[-file]` marker that lists
# no rules (only whitespace / commas after it). A marker with at least one token
# is handled by `suppression.unknown-rule` instead; prose is ignored.

value = 1 # rigor:disable
other = 2 # rigor:disable-file
