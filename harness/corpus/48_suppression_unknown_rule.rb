# `suppression.unknown-rule` fires for every `# rigor:disable[-file]` token that
# resolves to no known rule id, alias, family, or engine diagnostic. Anchored at
# the comment's `#`. Known tokens (family / all / legacy alias / non-check
# family / canonical id) stay silent.

ok1 = 1 # rigor:disable call.no-such-rule
ok2 = 2 # rigor:disable call.undefined-method,call.bogus-one, call.bogus-two
ok3 = 3 # rigor:disable call
ok4 = 4 # rigor:disable all
ok5 = 5 # rigor:disable undefined-method
ok6 = 6 # rigor:disable rbs_extended.something
ok7 = 7 # rigor:disable-next-line call.undefined-method
# this documents `# rigor:disable <rule>` usage and is not a suppression
