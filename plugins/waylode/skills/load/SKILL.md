---
name: load
description: Check host load before heavy builds and tests, and know what to do when Waylode refuses one. Use before running cargo, go, npm/bun/pnpm test, vitest or jest, before an unbounded grep, rg or go test, and whenever a command was refused for host load or the session brief says host load is elevated, severe or UNKNOWN.
license: MIT
compatibility: The refusal and the advice come from the waylode-hook command-gate (any Claude Code session with the Waylode plugin). It reads what the Waylode app publishes, so it speaks only while the app is running.
metadata:
  agent-plugins: "1.0.0"
---

# Waylode host load

On 2026-11-14 a Mac hung with swap at 0.9x RAM and a load of 120 on 18 cores, under agent sessions that were each running a build or a test. Waylode's app measures the machine and publishes one verdict. Your `command-gate` hook reads that verdict, never samples the machine itself, and acts on it before a heavy command runs.

## Before heavy work

1. Read the **`host load:`** line in the session brief. It names the tier (clear, elevated or severe), what is binding it (memory pressure, swap, the memory compressor, CPU load, heat, battery, free disk), and how many Claude and Codex sessions are running (launched by Waylode, observed elsewhere).
2. `UNKNOWN` means Waylode could not read a verdict (the app is not running, it is too old, or measurement is switched off). Heavy commands are then **not load-checked**: treat the host as possibly overloaded and keep them small.
3. Run one heavy command at a time. Do not start a build while another of yours is still running, and do not start parallel test runs.

## Keep it capped

| Work | Smaller form |
| --- | --- |
| `cargo build`, `check`, `clippy`, `test` | `CARGO_BUILD_JOBS=2 cargo …`; for tests also `-- --test-threads=2` |
| `go build`, `go test` | `GOMAXPROCS=4 go test -p 2 …`; always `-timeout` and `GOMEMLIMIT` for `go test ./...` or `-fuzz` |
| `jest` | `--maxWorkers=2` |
| `grep -r` | add `--exclude-dir` for build and dependency directories; never `-z`, `-a` or `--null-data` over a tree |
| `rg` | keep the ignore rules; if you need `-uu` or `--no-ignore`, add `-g '!target*'` and `--max-filesize 1M` |
| Python training or benchmark runs | bound memory first (a smaller batch or sample, or a `ulimit`) |

Pipe into a bounded reader (`head`, `tail`) rather than letting a command's whole output sit in memory. When you are finished with Podman, a VM or a dev server you started, stop it.

## When a command is refused

The reply says what is refusing (the tier and the resource), and that Waylode refused it. That is a fact about the machine, not a permission question.

- **Wait, then run it once.** A few minutes is the right time scale.
- **Never retry in a loop.** The fifth identical refusal in ten minutes becomes `STOP: you are looping`. Stop running heavy commands and tell the user the host is overloaded.
- **Never switch tools to dodge it.** Running the same work through another build tool, a wrapper script, a subshell or `bash -c` is the failure this prevents.
- Do what does not need the build in the meantime: read code, write the change, plan the tests.
- If the work cannot wait, tell the user. They can set **Settings → Agents → Host load → Command gate** to Advise (the same message, nothing refused) or Off.

`advise` mode and a verdict that could not be read never refuse: you only get a line saying so.

## Duplicate servers waste memory

Each session starts its own copy of every MCP server it is configured with. Several sessions each running a copy of the same server (desktop-commander and a PDF server were about 1.2 GB at five sessions) cost memory the builds need. Prefer a project-scoped configuration, and say so to the user instead of starting another copy.
