# The bare-class bucket, characterised — where the next slices are (2026-08-08)

After the 141-row [adjudication](20260807-gap-adjudication-141.md) retired three
clusters, the census's largest remaining actionable bucket is `call.undefined-
method` with a BARE CLASS receiver: **122 rows** (was 147; PRs #63/#64 took the
difference). This note samples it so the next slice is chosen from evidence
rather than from the bucket's size.

Measured on the verified post-merge census (`scratchpad/gaps-v2.json`, 1168
rows, pin `v0.3.1`).

| receiver | rows | corpora |
|---|---:|---|
| `String` | 33 | gitlab lib, mastodon |
| `Object` | 26 | mostly test-framework DSL (`expects` ×9, `count` ×4) |
| `OpenStruct` | 18 | concurrent-ruby — **one file** |
| `Hash` | 11 | |
| `Class` | 8 | |
| `Array` | 7 | |
| the rest (`Set`, `Numeric`, `Integer`, `IO`, `Monitor`, …) | 19 | |

Corpus split: gitlab lib 60, concurrent-ruby 36, mail 13, net-ssh 9, mastodon 3.

## `String` (33) — mostly ALREADY specced, not a new mechanism

Sampled sites resolve to mechanisms the narrowing arc already owns:

- `activitypub/case_transform.rb:18`, `seo/case_transform.rb:28` —
  `cache[v] ||= if … v.underscore …` — the **stage-3b unmodeled-form** shape
  ([evidence](20260807-narrowing-stage3-probe-evidence.md)); the `when String`
  narrowing is computed and then discarded.
- `api_error_formatter.rb:20` — `message.is_a?(String) && message.present?` —
  the **stage-3a `&&` conjunction** shape.
- `award_emoji.rb:14` — `awardable_params[:resource].to_s.singularize` — the
  cross-file block-param element typing already ruled out of the `to_s` slice
  (see the [class-narrowing spec](20260807-class-narrowing-slice-spec.md)
  § "Slice 2 is REFUTED"). Heavy mechanism, still deferred.
- `custom_attributes_endpoints.rb:9`, `statuses_helper.rb:32` — receiver typed
  from an RBS return / a method parameter; interprocedural.

So the String bucket is not an argument for a new slice — it is an argument for
finishing narrowing stage 3, which is already specced.

## `OpenStruct` (18) — one file, one mechanism, and closing it would be wrong

All 18 are in concurrent-ruby `examples/benchmark_read_write_lock.rb`, all on a
GLOBAL: `$options = OpenStruct.new` then `$options.threads = 100`,
`$options.interleave = false`, … The mechanism is global-variable typing, which
rigor-rs does not do at all.

Two reasons to leave it: the rows are one file of one corpus (no generality),
and the diagnostics are **wrong at runtime** — `OpenStruct` accepts any
attribute by design. Probed: both engines already fire on
`OpenStruct.new(threads: 4).threads`, so the port faithfully mirrors a
reference model that rejects correct Ruby. That is a shared defect rather than
a parity gap; it belongs in upstream feedback, not in a slice.

## `Object` (26) — test-framework DSL, adjudicate before building

`Object#expects` (mocha) ×9, `Object#count` ×4 and friends. These have the
smell of the adjudicated clusters (a DSL method the configless environment
cannot know), but they were NOT part of the 141-row pass. Adjudicate before
spending anything here.

## Recommendation

1. Finish **narrowing stage 3** — it owns the largest genuinely closable slice
   of this bucket (the `String` rows) plus its own 49 window-candidates.
2. Then **collection-shape stages 1–2**
   ([spec](20260807-collection-shape-slice-spec.md), 18 predicted rows).
3. **Adjudicate the `Object` bucket (26)** before treating it as actionable.
4. Do NOT build global-variable typing for the OpenStruct rows.
