/**
 * Remotes, as the UI sees them.
 *
 * Two things matter here beyond listing names. First, **a push URL that
 * differs from the fetch URL must be visible** — a repository configured to
 * fetch from upstream and push to a fork is a normal setup, and one configured
 * to push somewhere unexpected is a serious problem; both look identical if
 * the two URLs are collapsed into one field.
 *
 * Second, **credentials must never be rendered.** A remote URL may legally
 * carry `https://user:token@host/...`, and that token would otherwise be shown
 * on screen, copied into screenshots, and pasted into issue reports.
 */

/** Mirrors the Rust `RemoteInfo`. */
export interface RemoteInfo {
  name: string;
  fetch_url: string | null;
  push_url: string | null;
  tracking_branches: number;
  is_default: boolean;
}

/** Mirrors the Rust `RemoteChange`, an internally tagged enum. */
export type RemoteChange =
  | { kind: "add"; name: string; url: string }
  | { kind: "remove"; name: string }
  | { kind: "rename"; name: string; new_name: string }
  | { kind: "seturl"; name: string; url: string; push: boolean }
  | { kind: "prune"; name: string };

/** Wire shape of `cmd_list_remotes`. A bare array could not say when the cap cut remotes. */
export interface RemoteList {
  remotes: RemoteInfo[];
  truncated: boolean;
}

/**
 * Unwraps a `cmd_list_remotes` payload.
 *
 * A bare array or a missing `truncated` flag is a failed read, not "no
 * remotes". Folding those into an empty list is how a truncated config
 * comes to look like a local-only repository.
 */
export function parseRemoteList(value: unknown): {
  remotes: RemoteInfo[];
  truncated: boolean;
  failed: boolean;
} {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return { remotes: [], truncated: false, failed: true };
  }
  const rec = value as { remotes?: unknown; truncated?: unknown };
  if (!Array.isArray(rec.remotes) || typeof rec.truncated !== "boolean") {
    return { remotes: [], truncated: false, failed: true };
  }
  const remotes: RemoteInfo[] = [];
  for (const item of rec.remotes) {
    if (!item || typeof item !== "object" || Array.isArray(item)) {
      return { remotes: [], truncated: false, failed: true };
    }
    const r = item as {
      name?: unknown;
      fetch_url?: unknown;
      push_url?: unknown;
      tracking_branches?: unknown;
      is_default?: unknown;
    };
    if (typeof r.name !== "string" || typeof r.tracking_branches !== "number" || typeof r.is_default !== "boolean") {
      return { remotes: [], truncated: false, failed: true };
    }
    remotes.push({
      name: r.name,
      fetch_url: typeof r.fetch_url === "string" ? r.fetch_url : null,
      push_url: typeof r.push_url === "string" ? r.push_url : null,
      tracking_branches: r.tracking_branches,
      is_default: r.is_default,
    });
  }
  return { remotes, truncated: rec.truncated, failed: false };
}

/**
 * Strips userinfo from a URL for display.
 *
 * `https://user:ghp_secret@github.com/o/r.git` renders as
 * `https://user@github.com/o/r.git` — enough to see which account is
 * configured, without putting the token on screen. Non-URL remotes (local
 * paths, scp-style `git@host:path`) are returned unchanged: they carry no
 * password component, and mangling a path would misrepresent the remote.
 */
export function redactRemoteUrl(url: string): string {
  const match = /^([a-zA-Z][a-zA-Z0-9+.-]*:\/\/)([^/@]*)@(.*)$/.exec(url);
  if (!match) return url;
  const [, scheme, userinfo, rest] = match;
  const user = userinfo.split(":")[0];
  return user ? `${scheme}${user}@${rest}` : `${scheme}${rest}`;
}

/** True when this remote's URL carries an embedded password or token. */
export function carriesEmbeddedCredential(url: string | null): boolean {
  if (!url) return false;
  const match = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\/([^/@]*)@/.exec(url);
  return Boolean(match && match[1].includes(":"));
}

/** The URL pushes actually go to: the push URL when set, else the fetch URL. */
export function effectivePushUrl(remote: RemoteInfo): string | null {
  return remote.push_url ?? remote.fetch_url;
}

/**
 * True when this remote fetches from one place and pushes to another.
 *
 * Surfaced prominently: it is either a deliberate fork workflow the user wants
 * confirmed, or a misconfiguration that sends work to the wrong repository.
 */
export function hasSplitUrls(remote: RemoteInfo): boolean {
  return Boolean(remote.push_url && remote.push_url !== remote.fetch_url);
}

/** Host portion of a remote URL, for a compact secondary label. */
export function remoteHost(url: string | null): string | null {
  if (!url) return null;
  const scheme = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\/(?:[^/@]*@)?([^/:]+)/.exec(url);
  if (scheme) return scheme[1];
  // scp-style `git@github.com:owner/repo.git`
  const scp = /^(?:[^@/]+@)?([^/:]+):(?!\/)/.exec(url);
  if (scp) return scp[1];
  return null;
}

/**
 * Client-side validation, mirroring the backend's rules so the user is told
 * before a round trip. The backend still validates — this is a courtesy layer,
 * never the enforcement.
 */
export function validateRemoteName(name: string): string | null {
  const trimmed = name.trim();
  if (!trimmed) return "Enter a name for the remote.";
  if (trimmed.startsWith("-")) return "A remote name cannot start with '-'.";
  if (trimmed.includes("..")) return "A remote name cannot contain '..'.";
  if (/[\s~^:?*[\\\0]/.test(trimmed)) return "A remote name cannot contain spaces or ~^:?*[\\.";
  if (trimmed.startsWith(".") || trimmed.endsWith(".") || trimmed.endsWith("/")) {
    return "A remote name cannot start or end with '.' or end with '/'.";
  }
  if (trimmed.endsWith(".lock")) return "A remote name cannot end with '.lock'.";
  return null;
}

export function validateRemoteUrl(url: string): string | null {
  const trimmed = url.trim();
  if (!trimmed) return "Enter a URL for the remote.";
  if (trimmed.startsWith("-")) return "A remote URL cannot start with '-'.";
  // A codepoint loop rather than a regex range: the range was written with
  // literal control BYTES in the source, which are invisible in editors and
  // diffs and are silently rewritten by any tool that normalizes whitespace —
  // a change that would weaken this check without showing up in review.
  for (const char of trimmed) {
    const code = char.codePointAt(0) ?? 0;
    if (code < 0x20 || code === 0x7f) {
      return "A remote URL cannot contain control characters.";
    }
  }
  // `ext::`, `transport::` and friends run an arbitrary helper program.
  if (/^[a-zA-Z0-9+.-]*::/.test(trimmed)) {
    return "That transport is not supported. Use http(s), ssh, git, file, or a local path.";
  }
  return null;
}

/**
 * True for changes that can lose data or redirect work, and so need confirming.
 *
 * `remove` discards the remote-tracking branches it owns; `prune` deletes refs;
 * `seturl` silently redirects where every future push lands, which is the most
 * consequential of the three and the least obviously dangerous.
 */
export function isDestructiveRemoteChange(change: RemoteChange): boolean {
  return change.kind === "remove" || change.kind === "prune" || change.kind === "seturl";
}

/** What the change will do, for the confirmation body. */
export function remoteChangeConsequence(change: RemoteChange): string {
  switch (change.kind) {
    case "add":
      return `Adds '${change.name}'. Nothing is fetched until you fetch it.`;
    case "remove":
      return `Removes '${change.name}' and every remote-tracking branch under it. Your local branches and commits are untouched.`;
    case "rename":
      return `Renames '${change.name}' to '${change.new_name}' and moves its remote-tracking branches with it.`;
    case "seturl":
      return change.push
        ? `Every future push to '${change.name}' will go to this URL instead.`
        : `Every future fetch from '${change.name}' will come from this URL instead. Pushes follow it too unless a separate push URL is set.`;
    case "prune":
      return `Deletes remote-tracking branches under '${change.name}' whose upstream branch no longer exists. Local branches are untouched.`;
  }
}

/**
 * Human summary of a remote list.
 *
 * Distinguishes "no remotes" — a local-only repository, where push and pull
 * cannot work at all — from a normal configuration, because that is the reason
 * a first-time user's push fails.
 */
export function describeRemotes(remotes: readonly RemoteInfo[]): string {
  if (remotes.length === 0) {
    return "No remotes configured — this repository exists only on this machine.";
  }
  if (remotes.length === 1) {
    const only = remotes[0];
    const host = remoteHost(only.fetch_url);
    return host ? `1 remote — ${only.name} at ${host}` : `1 remote — ${only.name}`;
  }
  return `${remotes.length} remotes configured`;
}
