# rigor-rs#770 — the `recv[k]` read and the `h[k] ||= v` stored-slot record
# on verbatim-carrier unions. A union whose every member is a Hash/Array
# carrier (Tuple, HashShape) erases each member to `Hash[K,V]` / `Array[E]`
# and unions the `[]` answers (the reference's `dispatch_union` +
# `hash_shape_type_args` / `tuple_type_args`), so `h[k]` reads the element
# envelope — and `verbatim_slot?` admits the union for the `||=` record,
# whose `current` half is the same `[]` read.

# The issue rows: `h[k] ||= v` on an all-verbatim union records
# `narrow_truthy(h[k]) | v`, and a plain `h[k]` reads the envelope.
h1 = c ? { a: 1 } : [1]
h1[0] ||= 5
h1[0].zzz                    # for 1 | 5

h2 = c ? [1] : [2]
h2[0] ||= 5
h2[0].zzz                    # for 1 | 2 | 5

h3 = c ? { a: 1 } : { b: 2 }
h3[0] ||= 5
h3[0].zzz                    # for 1 | 2 | 5

x4 = (c ? { a: 1 } : [1])[0]
x4.zzz                       # for 1

h5 = c ? [1] : [2, 3]
x5 = h5[1]
x5.zzz                       # for 1 | 2 | 3 — envelope, not a memberwise slot lookup

h6 = c ? { a: 1 } : { a: 2, b: 3 }
x6 = h6[:a]
x6.zzz                       # for 1 | 2 | 3 — the :b value joins the envelope too

# A non-literal index reads the same envelope.
h7 = c ? { a: 1 } : [1]
i7 = c ? 0 : 1
h7[i7].zzz                   # for 1

# --- declines ----------------------------------------------------------------

# A union with any non-verbatim member declines the whole read.
h8 = c ? [1] : "s"
h8[0].zzz                    # silent both sides

# A negative Integer key on an Array member aliases a non-negative slot —
# `verbatim_slot?` declines, so `||=` records nothing.
h9 = c ? [1] : [2]
h9[-1] ||= 5
h9[-1].zzz                   # silent both sides

# Controls: the single-carrier rows keep their existing answers.
h10 = { a: 1 }
h10[:a] ||= 2
h10[:a].zzz                  # for 1 | 2

h11 = [1]
h11[0] ||= 5
h11[0].zzz                   # for 1 | 5
