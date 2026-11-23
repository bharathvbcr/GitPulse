# GitPulse Governance

GitPulse is a small project with one maintainer. This page says how decisions
get made, so contributors know what to expect.

## Roles

- **Maintainer.** [@bharathvbcr](https://github.com/bharathvbcr) is the sole
  maintainer and the project's final decision-maker (a "benevolent dictator"
  model). The maintainer reviews and merges pull requests, cuts releases,
  triages issues, handles security reports and enforces the
  [Code of Conduct](CODE_OF_CONDUCT.md).
- **Contributors.** Anyone who opens an issue, a discussion or a pull request.
  Every kind of contribution counts; see "Recognition" in
  [CONTRIBUTING.md](CONTRIBUTING.md).

## How decisions are made

- **Questions and open-ended ideas** start in
  [GitHub Discussions](https://github.com/bharathvbcr/GitPulse/discussions).
- **Scoped, actionable work** is an issue, opened from the
  [issue forms](https://github.com/bharathvbcr/GitPulse/issues/new/choose).
- **Code and documentation changes** are pull requests. The maintainer decides
  on the merits: does it fit what GitPulse is for, is it tested, does it keep
  the contracts in [CONTRIBUTING.md](CONTRIBUTING.md) (section 4) green. A change
  that alters behaviour, an IPC payload or a security boundary is discussed in an
  issue first.
- **Disagreement** is settled in the thread by the maintainer, who gives the
  reason. A declined proposal can be raised again with new evidence.
- **Security** reports follow [docs/SECURITY.md](docs/SECURITY.md), privately.

The maintainer aims to respond to new issues and pull requests within a week.
This is a goal for a one-person project, not a service level.

## Becoming a maintainer

There is no fixed quota. A contributor who has landed several non-trivial,
well-tested changes over a few months, has reviewed other people's work
well, and follows the Code of Conduct may be invited to become a maintainer, or
may ask in a Discussion. Maintainers are added by the current maintainer, are
given write access and a CODEOWNERS entry, and are listed here. Maintainers
who are inactive for a long period are moved to emeritus status.

## Branches and merging

- **`main` is protected by a ruleset** ("main: pull requests and checks").
  Contributors' changes land through pull requests, and the ruleset requires:
  - a pull request with one approving review, and review from a code owner
    (see [`.github/CODEOWNERS`](.github/CODEOWNERS));
  - stale approvals dismissed when new commits are pushed;
  - all review conversations resolved;
  - the required status checks: Rust Lint (ubuntu, macOS and Windows), Frontend
    (Windows), Gusset engine (Linux), and "No commit authored as a test fixture";
  - no force-push and no deletion of `main`.
  Merge, squash and rebase merges are all allowed.
- **The repository admin can bypass the ruleset.** The maintainer may push to
  `main` directly as repository admin, which is used for releases, for landing
  trains of merged work, and for automation. Everything else, including every
  contributor's change, is subject to the ruleset.
- **Release tags (`v*`) are protected:** they cannot be deleted, force-updated or moved.
- **Branch names** use a type prefix (`feat/`, `fix/`, `docs/`, `chore/`, ...).
- **Commits** use Conventional Commit subjects and carry a DCO sign-off
  (`git commit -s`). Sign-off is required by project policy and checked in
  review; GitHub does not enforce it automatically for commits made outside
  the web UI. Both are explained in [CONTRIBUTING.md](CONTRIBUTING.md).
- **Merge policy.** The maintainer merges once the checks are green and the
  review is approved. Pull requests are atomic: one coherent concern each.
  Because the maintainer is the only code owner, the ruleset's pull-request and
  review requirements apply to contributors; the maintainer's own changes still
  go through a pull request and the status checks.

## Releases

A release is a `vX.Y.Z` tag (beta tags are `vX.Y.Z-beta.N`) on a commit whose
`ci.yml` and `coverage.yml` runs on `main` have already succeeded. The
`release.yml` workflow builds a draft release; a person publishes it after the
asset set is verified. The steps, and how to take a release back, are in
"Releasing and rolling back" in [CONTRIBUTING.md](CONTRIBUTING.md), and the
history is in [CHANGELOG.md](CHANGELOG.md). Security fixes go to the latest
release only, as described in [docs/SECURITY.md](docs/SECURITY.md).

## Changing this document

Changes to governance are pull requests like any other, and the maintainer
decides after discussion in an issue.
