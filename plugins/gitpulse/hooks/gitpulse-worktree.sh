#!/bin/sh
# GitPulse's launcher for Claude Code's WorktreeCreate and WorktreeRemove hooks.
#
#   sh gitpulse-worktree.sh <gitpulse-hook binary> worktree-create|worktree-remove
#
# Claude Code hands the hook a JSON payload on stdin. A WorktreeCreate hook
# replaces Claude Code's own `git worktree add`: it prints the new worktree's
# path as the last stdout line and exits 0, or creation fails. So a hook that
# cannot even start would cost every `claude --worktree`, EnterWorktree and
# isolated subagent its worktree. This launcher is what stands between the two:
#
#   * When the GitPulse binary is runnable and serves the event, it does the
#     work (src-tauri/src/hooks/worktree.rs) and its exit code is final, except
#     75, which means it did nothing (the repository is not trusted in GitPulse,
#     or GitPulse could not do its part) and plain git should.
#   * Otherwise (not installed, not on PATH, older than this plugin, or 75), the
#     plain git Claude Code would have run is run here, without GitPulse's setup:
#     `git worktree add -b worktree-<name> <root>/.claude/worktrees/<name>` from
#     the local default branch, and for removal `git worktree remove`, with
#     --force only when the same checks GitPulse makes find work (Claude Code
#     has already confirmed such a removal with the user). The branch is kept,
#     so no commit becomes unreachable; a check that cannot run keeps the tree.
#
# stdout carries the created path and nothing else; every message goes to
# stderr. POSIX sh, so it needs a shell: on Windows that is the sh Git for
# Windows ships.
set -u

bin=${1:-}
event=${2:-}

say() { printf 'gitpulse-worktree: %s\n' "$*" >&2; }

case $event in
  worktree-create|worktree-remove) ;;
  *) say "unknown event '$event'; expected worktree-create or worktree-remove"; exit 1 ;;
esac

input=$(cat) || { say "could not read the hook input"; exit 1; }

# The string value of JSON key $1, only when it holds no escaped character.
# A value that needs unescaping is not guessed at: the caller refuses it.
field() {
  printf '%s' "$input" | tr '\n' ' ' |
    sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"\\\\]*\)\".*/\1/p"
}

# Whether $bin is runnable and lists $event among the subcommands it serves.
serves() {
  [ -n "$bin" ] || return 1
  command -v "$bin" >/dev/null 2>&1 || return 1
  "$bin" --version 2>/dev/null | sed -n 's/^subcommands: //p' | tr ',' '\n' |
    sed 's/^ *//; s/ *$//' | grep -qx "$event"
}

if serves; then
  printf '%s' "$input" | "$bin" "$event"
  status=$?
  [ "$status" -eq 75 ] || exit "$status"
  say "GitPulse did nothing (its reason is above); plain git will"
else
  say "$bin cannot serve $event (not installed, not on PATH, or older than this plugin); plain git will, without GitPulse's setup"
fi

# The repository's default branch as it is locally (`main`, not `origin/main`):
# the branch origin/HEAD names, else the first of main, master, trunk and
# develop that exists here or on origin. Nothing, meaning HEAD, when that
# branch does not exist locally. GitPulse's own path resolves the name through
# GitReader::default_branch_name; this is the same rule for the usual remote.
local_default_base() {
  dflt=$(git -C "$1" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
  dflt=${dflt#origin/}
  if [ -z "$dflt" ]; then
    for candidate in main master trunk develop; do
      if git -C "$1" show-ref --verify --quiet "refs/heads/$candidate" ||
        git -C "$1" show-ref --verify --quiet "refs/remotes/origin/$candidate"; then
        dflt=$candidate
        break
      fi
    done
  fi
  if [ -n "$dflt" ] && git -C "$1" show-ref --verify --quiet "refs/heads/$dflt"; then
    printf '%s' "$dflt"
  fi
}

create() {
  name=$(field name)
  case $name in
    '' | .* | -* | *..* | *.lock | *. | *[!A-Za-z0-9._-]*)
      say "refused: the hook input names no usable worktree ('$name')"
      exit 1
      ;;
  esac
  dir=$(field cwd)
  if [ -z "$dir" ] || [ ! -d "$dir" ]; then dir=$PWD; fi
  root=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null) ||
    { say "refused: $dir is not inside a git working tree"; exit 1; }
  root=$(cd "$root" && pwd -P) || { say "refused: cannot resolve $root"; exit 1; }
  for container in "$root/.claude" "$root/.claude/worktrees"; do
    if [ -L "$container" ] || { [ -e "$container" ] && [ ! -d "$container" ]; }; then
      say "refused: $container is not a directory (Claude Code refuses a worktree path through a link inside the repository)"
      exit 1
    fi
  done
  target=$root/.claude/worktrees/$name
  if [ -e "$target" ] || [ -L "$target" ]; then
    say "refused: $target already exists"
    exit 1
  fi
  base=$(local_default_base "$root")
  if [ -n "$base" ]; then
    git -C "$root" worktree add -b "worktree-$name" "$target" "$base" >&2
  else
    git -C "$root" worktree add -b "worktree-$name" "$target" >&2
  fi || { say "git failed: git worktree add did not create $target"; exit 1; }
  if [ ! -f "$target/.git" ]; then
    say "git reported success but $target is not a ready worktree"
    exit 1
  fi
  say "created $target on worktree-$name from ${base:-HEAD} with plain git"
  printf '%s\n' "$target"
}

remove() {
  path=$(field worktree_path)
  case $path in
    /*) ;;
    *)
      say "refused: the hook input names no absolute worktree_path this launcher can read; the worktree is kept"
      exit 1
      ;;
  esac
  if [ ! -e "$path" ] && [ ! -L "$path" ]; then
    say "$path is already gone"
    exit 0
  fi
  keep() {
    say "refused: $1. The worktree is kept. Commit, push or merge the work first, or remove it yourself with \`git worktree remove --force $path\`"
    exit 1
  }
  top=$(git -C "$path" rev-parse --show-toplevel 2>/dev/null) || keep "$path is not a git worktree"
  [ "$(cd "$top" && pwd -P)" = "$(cd "$path" && pwd -P)" ] || keep "$path is not the top of a git worktree"
  gitdir=$(git -C "$path" rev-parse --absolute-git-dir 2>/dev/null) || keep "cannot read its git directory"
  common=$(git -C "$path" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) ||
    keep "cannot read its repository"
  [ "$gitdir" != "$common" ] || keep "$path is the repository's main checkout, not a linked worktree"
  # Claude Code confirms with the user before it removes a worktree that holds
  # work, so work found here is discarded (--force), as GitPulse's own path
  # does. A check that cannot run still keeps the worktree.
  dirty=$(git -C "$path" status --porcelain=v1 --untracked-files=all --ignore-submodules=none) ||
    keep "could not read its status"
  branch=$(git -C "$path" symbolic-ref --quiet --short HEAD 2>/dev/null)
  if [ -n "$branch" ]; then
    case $branch in
      *[\*\?\[\\]*) keep "its branch '$branch' cannot be checked exactly for unpushed commits" ;;
    esac
    count=$(git -C "$path" rev-list --count HEAD --not "--exclude=$branch" --branches --remotes) ||
      keep "could not count its unpushed commits"
  else
    count=$(git -C "$path" rev-list --count HEAD --not --branches --remotes) ||
      keep "could not count its unpushed commits"
  fi
  force=
  if [ -n "$dirty" ]; then
    force=--force
    say "its uncommitted or untracked files are discarded, as Claude Code confirmed the removal"
  fi
  if [ "$count" != 0 ]; then
    force=--force
    say "$count commit(s) on no other branch and no remote stay reachable from ${branch:-nothing (detached HEAD)}"
  fi
  main=$(dirname "$common")
  git -C "$main" worktree remove $force "$path" >&2 || { say "git failed: git worktree remove kept $path"; exit 1; }
  say "removed $path with plain git${branch:+; branch $branch is kept}"
}

case $event in
  worktree-create) create ;;
  worktree-remove) remove ;;
esac
