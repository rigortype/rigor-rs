# ADR-0042 (qualified-key index migration): witnessing through QUALIFIED
# nested-stdlib names. Slice 2 CLOSED the SINGLETON cases — `ERB::Util` /
# `CGI::Util` class-method witnessing now matches the oracle byte-for-byte,
# including the short-key MERGE-collision split (no cross-contamination). The
# non-nested CGI contrast stays matching. Still open (later slice): the nested
# stdlib CLASS INSTANCE (`Process::Status` via `Process.wait2`, line ~38) —
# an instance-path witness through a qualified-class-typed value.
# Byte-for-byte against the oracle on (rule, line, column).

require "erb"
require "cgi"

# --- nested MODULE singleton (`ERB::Util`) ----------------------------------

# Valid nested-module method (vendored rbs declares `self?.html_escape`).
ERB::Util.html_escape("s")

# A Rails monkeypatch NOT in the vendored rbs — undefined on ERB::Util.
y = ERB::Util.html_escape_once("s")
y.frobnicate

# Straightforwardly undefined on the nested-module singleton.
ERB::Util.no_such_method

# --- the Util short-key MERGE collision (the ADR's own example) --------------
# ERB::Util and CGI::Util are method-disjoint in the vendored rbs; the oracle
# keeps them fully distinct. `pretty` is an INSTANCE method on CGI::Util, so
# the singleton spelling is undefined on BOTH.
CGI::Util.pretty("<HTML></HTML>")
ERB::Util.pretty("<HTML></HTML>")
CGI::Util.html_escape("s")

# --- non-nested contrast (`CGI`) — already matching, must not regress --------
CGI.escape("s")
CGI.no_such_method_xyz("s")

# --- nested stdlib CLASS (`Process::Status`) ---------------------------------
_pid, status = Process.wait2
status.exited?
status.frobnicate

# Bare short name `Status` resolves to nothing (both engines silent).
Status.new
Status.absent
