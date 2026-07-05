# call.possible-nil-receiver — the ADR-0039 Slice-1a Array.new-provenance source.
#
# Expected reference diagnostics:
#   (call.possible-nil-receiver, line 18): possible nil receiver: `size' is undefined on NilClass
#
# rigor-rs status: PARITY — the shape-tier Slice-1a provenance fire.
#
# `Array.new(300000){…}` has a constant size > ARRAY_NEW_TUPLE_LIMIT (16), so the
# reference keeps it `Nominal[Array]` (NOT a Tuple) — and `Nominal[Array]#[](Range)`
# is `Array?`. rigor-rs mints the same nilability from the syntactic provenance
# (`Array.new` with a constant size > 16), threaded on the tenv side so it survives
# from the OUTER method scope into the block where the slice + use live. `size` is
# present on Array (the non-nil arm), absent on NilClass, and unguarded ⇒ both fire.
def bench
  arr = Array.new(300000) { |i| i }
  [1].each do
    sub = arr[0..5]
    n = sub.size
    n
  end
end
