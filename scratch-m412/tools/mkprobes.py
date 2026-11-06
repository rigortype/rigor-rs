#!/usr/bin/env python3
"""Build the #279 depthprobe (dump/bench modes; source from proto/279-tools,
copied to M412/probe279/src/main.rs) against each exported tree under
M412/trees/<v>, each in its own CARGO_TARGET_DIR (release, 1.88).

Usage: mkprobes.py VARIANT...    -> M412/targets/probe279-<v>/release/depthprobe
"""
import os
import shutil
import subprocess
import sys

from common import M412

for v in sys.argv[1:]:
    d = os.path.join(M412, "probe279-" + v)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, "Cargo.toml"), "w") as f:
        f.write("""[package]
name = "depthprobe"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[[bin]]
name = "depthprobe"
path = "../probe279/src/main.rs"

[dependencies]
rigor-parse = { path = "../trees/%s/crates/rigor-parse" }
""" % v)
    shutil.copy(os.path.join(M412, "trees", v, "Cargo.lock"), os.path.join(d, "Cargo.lock"))
    env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(M412, "targets", "probe279-" + v))
    r = subprocess.run(["cargo", "+1.88.0", "build", "-q", "--offline", "--release"], cwd=d, env=env,
                       capture_output=True, text=True)
    print(v, "exit", r.returncode, r.stderr[-1500:] if r.returncode else "")
