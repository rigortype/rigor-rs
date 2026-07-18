# ADR-0042 gate (oracle matrix s4c/s4d): a TOPLEVEL project-sig class whose
# bare name collides with a nested-stdlib short key (`Status` vs
# `Process::Status`, `Instance` vs `Digest::Instance` — the gitlab defect-2
# lineage). The oracle isolates the namespaces completely: only the project
# sig's own surface resolves. rigor-rs's short-key reopen-union still MERGES
# the stdlib surface into the shadow class, so the stdlib-only members are
# silently accepted — pinned here as documented gaps the ADR-0042 migration
# must close (residual defect-2 unsoundness).

class Status
  def my_own_method
    1
  end
end

class Instance
  def clusters
    []
  end
end

s = Status.new
s.my_own_method
s.exited?
s.frobnicate

i = Instance.new
i.clusters
i.digest
i.frobnicate
