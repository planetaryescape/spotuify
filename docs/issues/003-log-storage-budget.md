# Daily retention does not bound log storage

Baseline: `origin/main` at `b21ab30dac8450e36f753e9a0e0c4ecdaa9b7b3e`, `crates/spotuify-daemon/src/logging.rs:44-55`. Seven daily files can each grow without a byte limit. The separate tail reader correction does not itself cap disk usage.

Reuse lead: mxr `origin/main` at `3f111fb79440068663286de804909e918467ab48`, `crates/daemon/src/logging.rs` 401 lines, introduced by `83b54c0a312c32f85bee9b9f301f617b28d14535`. Read the current mechanism and dependencies before copying.

Next action: establish a byte/file retention budget and adapt the proven cross-process rotation mechanism. Keep active writers and readers consistent. Do not truncate or remove the live user's logs during investigation.

Done when concurrent writers cannot exceed the documented bounded retention window indefinitely, rotation survives restarts, and log tail still reports clear bounded output. Use fixture directories and boundary tests.
