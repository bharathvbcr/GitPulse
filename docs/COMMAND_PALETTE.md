# Command palette

The palette is a lazy-loaded entry point to GitPulse's existing stores, dialogs and
views. Open it with Cmd/Ctrl+K, the native menu, or the status bar. App handles the
first keyboard/event request before the component exists; subsequent requests use
the mounted palette. Clone and Rebase delegate to the same App functions as the
native menu. No additional package or backend command is required.

## Ownership and extensions

- `src/lib/palette/model.ts`: typed item contract, eight mode definitions, bounded
  query parsing, local relevance ranking, validated optional history and action outcomes.
- `src/lib/palette/catalog.ts`: global/repository commands, contextual availability,
  navigation from `VIEW_REGISTRY`, and host dialog callbacks. All sections get a
  command, including sections without an explicit palette label.
- `src/lib/palette/search.ts`: the existing files/code-intelligence IPC adapters under
  one debounced, deadline-bound lifecycle; exact workspace registry resolution.
- `src/lib/components/CommandPalette.svelte`: presentation, selection, paging,
  focus, request lifecycle and execution. It derives rows from live repository state.
- `src/App.svelte`: lazy loading and host-owned dialog state.

Add an application action to `buildCommands` with a stable ID, label, category,
optional description/keywords/shortcut, explicit availability reason, and a callback
to its existing owner. Return asynchronous mutation outcomes so refusal is visible.
Set `keepOpen` for a search-mode transition; set `closeBefore` for a follow-on dialog.
The latter removes the palette and waits for Svelte's DOM update before opening the
dialog, preventing focus restoration from stealing focus back. All other actions
close only on successful completion. Reopening during an accepted action is allowed,
but execution remains locked until that action settles, and its old completion
cannot close the newly opened palette.

Navigation to an application view or section belongs in `VIEW_REGISTRY`; do not
create a parallel view list. Host-owned dialogs receive callbacks via
`PaletteHostActions`. A host missing a callback gets an unavailable command with
an explanation.

## Search and state contracts

| Prefix | Source | Completeness |
| --- | --- | --- |
| `>` or none | Command catalog and open/recent repositories | All matched commands, paged by 50 |
| `/` | `cmd_list_repo_files` | All returned tracked/untracked non-ignored paths; the backend rejects inventories above its file cap |
| `%` | Open tabs, recent paths and tab actions | Bounded by the workspace store's existing limits |
| `#` | Active repository's loaded graph commits | Loaded history only; older history and unavailable history are disclosed |
| `@` | Active repository's branch snapshot | Current branch and unavailable checkout states are explained |
| `:` | `cmd_codeintel_search` | Producer counts, truncation and incomplete-walk notices retained |
| `::` | `cmd_workspace_search` plus `cmd_workspace_list` | Producer ranking, unavailable repositories and truncation retained |
| `?` | Mode descriptions, shortcuts and Map navigation | Help text is itself searchable; selecting a mode stays open |

Local matching accepts unordered whitespace-separated tokens across the name,
description, path, category and keywords. It reuses the existing fuzzy matcher and
highlighter, with label matches weighted above metadata and fuzzy matches. Usage
only breaks relevance ties. Workspace TF-IDF name ranking (`::query~`) stays in
producer order and is not refiltered with a literal substring predicate.

Queries are capped at 256 characters. Remote searches debounce for 180 ms and have
a 12-second UI deadline. Opening, retrying, switching repository generation, or
switching provider creates a new lifecycle. File-name typing filters the inventory
already returned rather than fetching it again. Cleanup invalidates callbacks and
cancels queued timers. The search IPC commands do not accept cancellation tokens:
an already-running backend call may finish after UI cancellation and is ignored.

History uses the existing `gitpulse_palette_frecency` key, migrates legacy numeric
counts, validates parsed values, and retains the most recent 120 records. It rejects
oversized stored blobs. Blocked local storage and quota errors retain session-only
functionality. Only completed actions enter history; search-mode changes, disabled
actions, cancellation and refusal do not.

Repository mutations continue through `repoStore` and existing native policy gates.
Palette availability is a usability check, not a replacement for backend validation.
Workspace symbols resolve an exact unique registry name to its root and verify the
active repository after opening. An unresolved, ambiguous or failed target never
falls through to selecting that path in the previous repository.

## Regression evidence

Before implementation, the rendered harness reproduced these four failures:

1. A Help mode action closed the palette.
2. An older symbol request replaced a newer completed result.
3. A failed workspace search rendered as no matching results.
4. Selecting a workspace symbol left the original repository active.

The same cases now pass. The harness additionally checks bounded/paged rendering,
full-inventory file search, no repeated file fetch on typing, duplicate execution,
failed navigation and mutation outcomes, disabled states, malformed history, IME,
keyboard navigation, focus restoration, prompt handoff and cold-start event wiring
(the App source contract). Unit tests exercise 100,000 file candidates, provider
timeouts/retry/cancellation, corrupt storage, registry ambiguity, command coverage
of every view/section, and callback routing to existing stores.

Run:

```sh
npm test -- src/lib/palette src/lib/components/CommandPalette.test.ts src/lib/components/CommandPalette.tools.test.ts src/App.test.ts
npm run test:browser -- --harness palette
npm run test:webkit -- --harness palette
npm run check
npm run coverage
npm run check:ipc
npm run check:types
npm run build
```

The browser harness mounts production components with explicit IPC fixtures; it
does not perform Git operations against real repositories. WebKit exercises the
macOS rendering engine. These checks do not prove an installed Tauri build's native
menu dispatch, file editor navigation, signing or release readiness.

Svelte lifecycle cleanup follows the [official effect documentation](https://svelte.dev/docs/svelte/$effect).

### Verified on September 9, 2026

- Full frontend/coverage run: 396 files passed; 5,224 tests passed and one existing
  test skipped. Coverage: 94.08% statements, 88.68% branches, 95.68% functions,
  96.05% lines; all configured thresholds passed.
- Palette browser harness: 44/44 in Chrome and 44/44 in macOS WKWebView. Existing
  diagnostics browser harness: 24/24. Dark desktop and light 390px layouts inspected.
- Svelte and TypeScript checks: zero errors/warnings. IPC command and wire contracts
  passed. Production frontend build passed with its large-bundle warning.
- The palette's production code is 660 lines across four owners, replacing the
  former 753-line component. Tests and documentation expand the overall patch.
- Rust tests, packaged Tauri installation, real Git mutations and release/signing
  checks were not run: this change modifies the frontend and browser harness only.
