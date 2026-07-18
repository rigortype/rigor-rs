# ADR-0042 gate (oracle matrix s1/s1b/s3/s7): witnessing through QUALIFIED
# nested-stdlib names. rigor-rs's index registers nested RBS declarations by
# SHORT key, so every qualified-path case below is a documented coverage gap
# (the GO-slice-5 surface); the non-nested CGI contrast pins the already-
# matching baseline the migration must not regress. Byte-for-byte against the
# oracle on (rule, line, column).

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
