# Ecosystem Open-Source Legal & Contribution Checklist

This document summarizes the unified licensing and contribution posture across
the public ecosystem repositories (`GitPulse`, `DevCouncil`, `Manvi`, `Jarvis`,
`gusset`, and `MarkDev`) and how it separates cleanly from hosted/source-available
services such as `gitpulse-relay`.

## 1. Dual License (`MIT OR Apache-2.0`)

Every public ecosystem repository is dual-licensed under **MIT OR Apache-2.0**
(disjunctive `OR` — downstream users choose either license):

- `LICENSE-MIT` — Canonical OSI/SPDX MIT License (`Copyright (c) 2026 Bharath Chandra Vaddaram`).
- `LICENSE-APACHE` — Canonical Apache License, Version 2.0 (201 lines, unmodified).
- `LICENSE` — Concise dual-license pointer naming both `LICENSE-MIT` and
  `LICENSE-APACHE` plus the Apache-2.0 §5 inbound=outbound contribution clause.
  Kept at repository root because npm packaging (`DevCouncil/package.json`),
  README links, and `TRADEMARKS.md` reference `LICENSE` directly.
- `NOTICE` — Apache-2.0 §4(d) attribution and third-party notices file.
- `DCO.txt` — Verbatim Developer Certificate of Origin Version 1.1 text.
- Package manifests declare SPDX `"MIT OR Apache-2.0"` (`Cargo.toml`, `package.json`,
  and plugin manifests).

### GitHub Licensee Detection Note

GitHub uses [Licensee](https://github.com/licensee/licensee) to populate the
repository sidebar license link and API `license` field:

- With unmodified `LICENSE-MIT` and `LICENSE-APACHE` at root alongside a short
  dual-license `LICENSE` pointer, GitHub renders a **"View license"** link in
  the sidebar while automated single-license API fields may report `Other` /
  `NOASSERTION` (which is standard for disjunctive dual-licensed repositories).
- Never merge both license texts into a single `LICENSE` file or alter the body
  of `LICENSE-MIT` / `LICENSE-APACHE`; keep those two files canonical.
- Always declare `MIT OR Apache-2.0` explicitly in `Cargo.toml`, `package.json`,
  and `README.md` so Cargo, npm, SBOM scanners, and human auditors parse the
  disjunctive choice unambiguously.

## 2. Contribution Model: Apache-2.0 §5 + DCO 1.1 (Not a Heavyweight CLA)

We intentionally choose **Apache-2.0 §5 inbound=outbound + DCO 1.1** over a
heavyweight Contributor License Agreement (CLA):

| Mechanism | Purpose | Friction |
| --- | --- | --- |
| **Apache-2.0 §5 (extended to dual `MIT OR Apache-2.0`)** | Ensures every submitted PR grants copyright and patent rights under both `MIT` and `Apache-2.0` without extra paperwork. | Zero |
| **DCO 1.1 (`git commit -s` / `Signed-off-by:`)** | Creates an auditable per-commit provenance record that the contributor authored or has rights to submit the change. | Very low |
| **Heavyweight CLA / Copyright Assignment** | Only required if relicensing community contributions under a proprietary license inside the same repo. | High (detracts casual and corporate contributors) |

### Enforcing DCO 1.1 on GitHub

To enforce `Signed-off-by:` automatically on pull requests without blocking
contributors with legal forms:

1. Install the [DCO GitHub App](https://github.com/apps/dco) on the public
   repositories (or add a lightweight workflow step checking every commit in
   `github.event.pull_request` for a `Signed-off-by:` trailer).
2. Keep the remediation commands in `CONTRIBUTING.md` (`git commit --amend -s`
   and `git rebase --signoff main`).

## 3. Open-Core & Trademark Boundaries

- **Code vs. Brand:** Permissive code licenses (`MIT OR Apache-2.0`) do not
  grant trademark rights. Keep [TRADEMARKS.md](../TRADEMARKS.md) explicit about
  the `GitPulse` name and logo.
- **Public OSS vs. Hosted Relay:** Hosted/commercial relay services (such as
  `gitpulse-relay`, licensed under `FCL-1.0-MIT`) live in separate repositories
  with their own license files. Never mix FCL/proprietary files into the public
  `MIT OR Apache-2.0` repositories.
- **Vendored & Pinned Sub-Workspaces:** Third-party vendored crates
  (`src-tauri/framework/`, `src-tauri/vendored/`) retain their upstream licenses
  documented in [NOTICE](../NOTICE). Cross-repo mirrored manifests (such as
  `Manvi/crates/Cargo.toml`, checked by `crate_pins_test.go` against a pinned
  DevCouncil commit) update their `license` field in the same change that bumps
  the upstream pin.
