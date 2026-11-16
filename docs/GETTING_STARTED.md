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

The guided walkthrough can take you through the controls. It opens by itself on a
first run; **Settings → Appearance → Guided walkthrough → Replay Tour** resumes a
deferred tour or replays a finished one.

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

For a parked merge or rebase, open **Work → Overview**, which leads with the conflict
editor while one is parked. Review the conflict chunks
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

GitPulse works without any of these. The code-map views (Health, Suspects, diff
impact and palette symbol search) stay open without DevMap and say what is
missing, with a **Set up code map** or **Build index** button in place; the
Manvi card and status pill do the same for Manvi.

- **Connect your agent:** **Settings → Agents → Connect your agent** (also on the
  first-run card and the setup wizard's last step) links the `gitpulse-mcp` and
  `gitpulse-hook` that ship inside the app into `~/.local/bin` and registers the
  MCP server with Claude Code and Codex through their own `mcp add`. It shows
  the plan first and changes nothing until you apply it; anything already set
  up differently is left alone and reported, with the commands to switch it.
  **Copy setup prompt** gives your agent the same steps to carry out instead,
  plus the optional Claude Code plugin that adds GitPulse's hooks and skills.
  The plugin also provides the `gitpulse` server, so use it or the direct
  registration, not both; Connect skips the Claude Code registration when the
  plugin is enabled. No source checkout or Rust toolchain is needed.
- **GitHub:** install and authenticate `gh` for pull requests, issues, and Actions.
- **Code Map:** use **Help → Set Up Optional Tools** for DevMap, the default
  module. There is no prebuilt DevMap this version can read yet, so the wizard
  builds it from source, which needs a Rust toolchain.
- **AI assistance:** configure a local model server for built-in assistance;
  supported Mac builds also offer Apple Intelligence for task drafting and, for
  commit messages, to phrase a subject the app has already classified. With no
  model at all, a commit message is still written from the staged change.
- **Agents and policy:** set up Manvi from the same wizard (it builds from
  source, which needs a Go toolchain; no published Manvi release reads this
  version's task store yet) and configure the provider you intend to use.
  Without Manvi, Git actions run unchecked and say so. Explicit agent sessions
  have their own permissions and network behavior.

Ordinary terminal sessions are separate from built-in AI suggestions. The
[terminal guide](TERMINAL.md) explains session persistence and recovery.

## Read a result's status

A missing coverage report is not zero coverage. An unavailable scanner is not a
clean audit. An unchecked policy decision is not an allowed verdict. Read errors,
scan age, loaded/total counts, and truncation notices before acting on a result.

For help, use [Troubleshooting](../wiki/Troubleshooting.md) and include the app
version, OS, exact error, and reproduction steps when reporting an issue. See
[open qualification](https://github.com/bharathvbcr/GitPulse/blob/90362c2eaee356294574da6427a637c048fbcf18/docs/QUALIFICATION.md) for platform and provider checks that
remain separate from local tests.
