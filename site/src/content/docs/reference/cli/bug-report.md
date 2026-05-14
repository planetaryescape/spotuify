---
title: "spotuify bug-report"
description: "Bundle a redacted diagnostic tarball for bug reports"
---

<!-- generated: spotuify-cli-reference -->

## When to use it

Bundle a redacted diagnostic tarball for bug reports

## Examples

```bash
spotuify bug-report --log-lines 500 --output spotuify-report.tar.gz
```

## Help

```text
Bundle a redacted diagnostic tarball for bug reports

Usage: spotuify bug-report [OPTIONS]

Options:
      --log-lines <LOG_LINES>  Last N log lines to include [default: 200]
      --output <OUTPUT>        Output path. Defaults to ./spotuify-bug-report-<ts>.tar.gz
      --log-format <LOG_FORMAT>  Pick log format for this run; also honoured via SPOTUIFY_LOG_FORMAT [possible values: text, json]
      --no-daemon-start          Never auto-start the daemon; fail with a daemon-unavailable hint instead
  -o, --set <key.path=value>     One-shot TOML override for this invocation only; repeatable
  -h, --help                     Print help
```
