# #805 — stale `scanner.rb` cite on `declaration()` (PR #828)

Doc-only. `collect.rs` `declaration()` cited `scanner.rb:201`; at pin
bbea1bac that is the blank line between `harvest_class_body_macro` and
`uniqueness_validator?`. The ported `declaration?` is `:274` (reference
comment mirrored by the port doc sits at 271–273).

Review follow-up: three more cites in the same file drifted — filed as
#830 rather than widening the PR.
