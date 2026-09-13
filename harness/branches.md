# Branch copy and merge

Open `/harness/branches.html` using the project's `npm run dev` server for an
interactive preview. Add `?theme=light` to inspect the light theme.
The page mounts the production branch sidebar, merge dialog, and notifications
with explicit IPC fixtures. Its merges do not modify real repositories.

Run the interaction regressions with:

```sh
npm run test:browser -- --harness branches
npm run test:webkit -- --harness branches
```

The checks open with the two-line branch row, which is geometry and so can
only be measured where real layout exists: each row is exactly as tall as the
window math believes, headers stay one line, neither line paints under the
hover actions or past the row's edge, the name owns line one and is shown in
full, and line two clips — author first, never a count. A stale branch shows
its tinted age rather than a chip. Fixture branches carry churn and divergence
derived from their names, and `release/0.0.9` is deliberately 120 days old;
without both, none of that chrome renders and the layout it used to break
cannot be seen.

The remaining checks cover full-name copying and denial feedback, keyboard focus and
activation, context-menu preselection, local and remote source selection,
fast-forward mode, fully qualified refs at the merge command boundary,
in-progress controls, error retention, conflict navigation, deleted sources,
changed destinations and repositories, detached HEAD, unavailable operation
checks, empty results, and searching beyond the displayed result limit.
Local and Remote filters retain the reviewed source; Show selected reveals it
even beyond the result cap, while Clear selection disables the merge action.
Branch choices form one radio group: arrow keys select, and Tab skips to merge
options. The shared focus helper respects negative tab indices on native controls.

The destination is the checked-out branch. To merge into another destination,
check it out first, then open Merge. A merge selects one source branch at a time.
Both the visible button and the branch context menu use the same review dialog
and existing guarded store command. No new native command or dependency is added.

Browser and WebKit fixture checks verify rendered interactions and command
arguments. They do not establish installed-app clipboard permissions or prove
a real native merge. Full Rust CI and app installation are separate checks.
