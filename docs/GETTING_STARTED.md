# Your first repository

[Documentation index](README.md) · [Installation](INSTALLATION.md) · [Feature reference](FEATURES.md)

## 1. Open and trust a repository

Install GitPulse and Git, launch the app, and use the Open control to choose a
repository. Read **Trust and Open** before approving it: repository hooks,
helpers, and project tools can run under your account. Trust is not an OS
sandbox or a guarantee about the repository's contents.

Canceling the trust prompt does not open the repository. If the OS denies folder
access, see [Onboarding and permissions](ONBOARDING.md). Repository trust and OS
permissions are separate decisions. The repository tab's **Revoke repository
trust** action closes access when you no longer want to trust it; the full scope
is documented in [Security](SECURITY.md).

**Walkthrough** in the title bar can guide you through the controls. You can defer
it and resume later, or replay it after completion.

## 2. Find the right view

| Question | Go to |
| --- | --- |
| What is in flight or blocked? | **Work → Overview** |
| What changed in this commit? | **History → Graph**, select a commit, then **Diff** |
| Where is this code, and who changed it? | **Code → Explorer**, then **Blame** |
| What do the repository's measurements say? | **Insights → Pulse / Coverage / Health / Storage** |

Use `⌘K` / `Ctrl+K` to open the command palette. Search by name, or use its
[prefix modes](COMMAND_PALETTE.md) for files, commits, branches, and symbols.
Native **Go** menus also open sections directly.

## 3. Review a change

Open **History → Graph** and select a commit. Switch to **Diff** to inspect its
files without losing the selected commit. Choose unified or side-by-side layout,
search the diff, and use the file rail to move between changes.

For your uncommitted work, review staged and unstaged changes before staging.
Selective patch staging is available in the diff. Check the staged result before
committing; a failed or partially completed operation needs its reported recovery
step, not an assumption that everything succeeded.

For a parked merge or rebase, open **Work → Resolve**. Review the conflict chunks
and remaining operation state before continuing. See the [feature reference](FEATURES.md)
for stash, worktree, stack, and recovery controls.

## 4. Capture the next task

Open **Tasks** beside Fleet for global or saved-workspace boards, or **Work → Tasks**
for the current repository. These scopes share task records.

Start with a sentence in Quick add. For example:

```text
Review parser edge cases #testing :: Cover empty and duplicate inputs
```

The parsed chips show the fields before creation. Use the task sheet for detail,
or the board's handoff action to choose an agent, checkout, and permissions.
Drafting suggestions require review and acceptance. Read
[Tasks and workspaces](TASKS_AND_WORKSPACES.md) for the complete token syntax,
loaded-page filtering, multi-selection, archive, and run boundaries.

## 5. Add optional capabilities when you need them

- **GitHub:** install and authenticate `gh` for pull requests, issues, and Actions.
- **Code Map:** use **Help → Set Up Optional Tools** for DevMap, the default module.
- **AI assistance:** configure a local model server for built-in assistance;
  supported Mac builds also offer Apple Intelligence for task drafting.
- **Agents and policy:** configure Manvi and the provider you intend to use.
  Explicit agent sessions have their own permissions and network behavior.

Ordinary terminal sessions are separate from built-in AI suggestions. The
[terminal guide](TERMINAL.md) explains session persistence and recovery.

## Read a result's status

A missing coverage report is not zero coverage. An unavailable scanner is not a
clean audit. An unchecked policy decision is not an allowed verdict. Read errors,
scan age, loaded/total counts, and truncation notices before acting on a result.

For help, use [Troubleshooting](../wiki/Troubleshooting.md) and include the app
version, OS, exact error, and reproduction steps when reporting an issue. See
[open qualification](QUALIFICATION.md) for platform and provider checks that
remain separate from local tests.
