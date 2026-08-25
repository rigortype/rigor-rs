# The bodies the `sig/declared.rbs` envelopes bound. See that file for what each
# annotation means and why the declared lane is graded as an exact match.

class Declared
  def formats(n)
    "n=#{n}"
  end

  def load_row(id)
    ROWS.fetch(id, "")
  end

  # Exceeds its `io.db` envelope: the `puts` is `io.output.stdout`, which the
  # bound does not admit.
  def load_and_log(id)
    row = load_row(id)
    puts row
    row
  end

  def unannotated
    nil
  end

  ROWS = { 1 => "one" }.freeze
end
