# #830 — stale `scanner.rb` cites in `collect.rs` (PR #831)

Doc-only. Three `scanner.rb` line cites drifted at pin bbea1bac:

- `TOP_LEVEL_KEY`: `:43` → `:50`
- `DECLARATION_MACROS`: `:48` → `:55`
- `block_parameter_name`: `:283` → `:343` — the `def`; line 283 is the
  `synthesize_accessors` call. The *other* `:283` cite (on
  `synthesize_accessors`, `collect.rs:723`) stays — it points at the
  call site, which is what that comment describes.

Remaining `scanner.rb:` cites in the file audited at the pin; all
resolve. No follow-up filed.
