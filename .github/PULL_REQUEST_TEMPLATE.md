## What this changes

<!-- One or two sentences. What behaviour is different after this PR? -->

Closes #

## Why

<!-- The problem being solved. If this fixes a bug, what was the root cause? -->

## Verification

**Required — every box must be genuinely checked, not assumed:**

- [ ] `npm run ci:local` passes locally.
      <!-- This runs: npm run check, npm test, npm run build, cargo fmt --check,
           cargo clippy -D warnings, cargo test. If you ran the pieces separately,
           say so below rather than ticking this. -->
- [ ] `npm run check:ipc` reports zero drift.
      <!-- Only meaningful if you added, removed, or renamed a cmd_* handler,
           but it is cheap and it is what CI runs. -->
- [ ] I manually exercised the change in `npm run tauri dev`.

**If this is a bug fix:**

- [ ] I added a test that **fails against the unfixed code** and passes now.
      <!-- Not "a test exists nearby" — a test that would have caught this bug. -->

**If this changes an IPC payload shape:**

- [ ] The Rust struct and the TypeScript interface agree field for field.
- [ ] `npm run check:types` passes.

**If this changes versioned manifests:**

- [ ] `npm run check:release` passes (`package.json`, `tauri.conf.json`,
      `Cargo.toml`, and `Cargo.lock` all name one version).

**Documentation:**

- [ ] User-visible behaviour is reflected in the README or `docs/`.
- [ ] New public Rust items and non-obvious TypeScript functions carry doc comments
      explaining *why*, not just *what*.
- [ ] No new dependency — or, if there is one, it is named below with justification.

## Anything reviewers should look at closely?

<!-- Trade-offs you made, alternatives you rejected, parts you are unsure about.
     "Nothing" is a valid answer. Flagging a weak spot is not a mark against the PR;
     it is the fastest route to a useful review. -->

## Screenshots or recordings

<!-- Required for visual changes. Before/after is ideal. Delete if not applicable. -->
