/**
 * Handing a saved task to a coding agent.
 *
 * The launch itself still goes through `runs.prepare_*`; this module decides
 * what the form should already say when it opens, so the common case is one
 * button rather than four choices and a path typed from memory.
 *
 * ## The checkout
 *
 * A run needs a working checkout, and the panel used to start with that field
 * empty every single time — even though the application already knows the
 * answer twice over. A registered repository's `identity_key` is
 * `local:{git common dir}`, and the repository tabs the reader has open are
 * real working checkouts. `checkoutCandidates` offers both, labelled by where
 * each came from, because they are not equally trustworthy: an open tab is a
 * directory GitPulse opened and read, while the folder beside a common dir is
 * an inference that is wrong for a bare repository and for a linked worktree
 * whose main checkout has moved.
 *
 * ## Why bypass is never remembered
 *
 * Every other choice here is a convenience. `bypass` turns off the agent's
 * permission checks and its sandbox, and `sanitizeHandoff` deliberately
 * refuses to restore it: a mode that disables a safety control has to be
 * chosen deliberately, per launch, with the acknowledgement the panel asks
 * for. Remembering it would silently widen access on the next launch, which
 * is exactly the change a preference must not be able to make on its own.
 */

import { PERMISSION_MODES, type PermissionMode, type RunKind } from "./vocabulary";
import { identityCommonDir, tabMatchesRegistered, type OpenTabRef, type RegisteredRef } from "./openMembership";
import { identityKey, normalizeRepoPath, type PathIdentityOptions } from "../repos/paths";

export type AgentProvider = "claude" | "codex";

export interface HandoffSettings {
  provider: AgentProvider;
  kind: RunKind;
  permission: PermissionMode;
}

export interface CheckoutCandidate {
  path: string;
  label: string;
  /**
   * `open` — a repository tab GitPulse has open at this path.
   * `derived` — the folder beside the repository's git common directory.
   */
  source: "open" | "derived";
}

/** Longest checkout path accepted, matching the workbench's own input cap. */
export const MAX_CHECKOUT_LENGTH = 4_096;

export function defaultHandoff(): HandoffSettings {
  return { provider: "codex", kind: "external_terminal", permission: "ask" };
}

export function isAgentProvider(value: unknown): value is AgentProvider {
  return value === "claude" || value === "codex";
}

export function isRunKind(value: unknown): value is RunKind {
  return value === "external_terminal" || value === "managed";
}

/**
 * A remembered handoff, made safe to restore.
 *
 * Three rules, and each one closes a hole a plain `JSON.parse` would leave:
 * an unknown provider or mode falls back to the default; `bypass` is never
 * restored; and a managed run is only offered for the provider that supports
 * one, so a stored `{claude, managed}` cannot arrive at a launch button that
 * would fail.
 */
export function sanitizeHandoff(value: unknown): HandoffSettings {
  const base = defaultHandoff();
  if (!value || typeof value !== "object") return base;
  const raw = value as Partial<HandoffSettings>;
  const provider = isAgentProvider(raw.provider) ? raw.provider : base.provider;
  const kind = isRunKind(raw.kind) && supportsManaged(provider) ? raw.kind : "external_terminal";
  const permission = PERMISSION_MODES.includes(raw.permission as PermissionMode) && raw.permission !== "bypass"
    ? (raw.permission as PermissionMode)
    : base.permission;
  return { provider, kind, permission };
}

/** Only Codex has a managed connection today; Claude Code is terminal-only. */
export function supportsManaged(provider: AgentProvider): boolean {
  return provider === "codex";
}

/**
 * The handoff a menu choice starts from.
 *
 * The context menu offers a provider and a connection; the permission mode
 * carries over from the last launch, because that is the choice a reader
 * makes once and the one they would be annoyed to re-make. `reconcileHandoff`
 * then drops an impossible pair rather than letting it reach a launch button.
 */
export function handoffFromTarget(
  target: { provider: AgentProvider; kind: RunKind },
  remembered: HandoffSettings,
): HandoffSettings {
  return reconcileHandoff({
    provider: target.provider,
    kind: target.kind,
    permission: remembered.permission,
  });
}

/** Keeps a settings object coherent after one field changes. */
export function reconcileHandoff(settings: HandoffSettings): HandoffSettings {
  return {
    ...settings,
    kind: supportsManaged(settings.provider) ? settings.kind : "external_terminal",
  };
}

export const PROVIDER_LABELS: Record<AgentProvider, string> = {
  claude: "Claude Code",
  codex: "Codex",
};

export function describeHandoff(settings: HandoffSettings): string {
  const connection = settings.kind === "managed" ? "managed" : "terminal";
  return `${PROVIDER_LABELS[settings.provider]} · ${connection}`;
}

/**
 * A typed or stored checkout path, trimmed and bounded, or "".
 *
 * Validity is delegated to `normalizeRepoPath`, which already owns what
 * counts as a usable path (control characters, empty, bare separators) — but
 * the *original* trimmed text is what is returned and sent onward. The
 * normalizer rewrites `\` to `/` and collapses separators, which is right for
 * comparing two paths and wrong for handing one to a shell on Windows.
 */
export function normalizeCheckout(value: unknown): string {
  if (typeof value !== "string") return "";
  const text = value.trim();
  if (!text || text.length > MAX_CHECKOUT_LENGTH) return "";
  return normalizeRepoPath(text) ? text : "";
}

/**
 * The working checkouts this repository could be launched in, best first.
 *
 * Open tabs come first and in the order the reader opened them, because
 * those are directories the application has actually resolved. The folder
 * beside the git common directory is appended only when nothing already
 * covers it, and is marked `derived` so the panel can say where it came from
 * rather than presenting a guess as a fact.
 */
export function checkoutCandidates(
  repositoryId: string,
  repositories: readonly RegisteredRef[],
  openTabs: readonly OpenTabRef[],
  options: PathIdentityOptions,
): CheckoutCandidate[] {
  const repository = repositories.find((entry) => entry.id === repositoryId);
  if (!repository) return [];
  const out: CheckoutCandidate[] = [];
  const seen = new Set<string>();
  // `identityKey`, not `normalizeRepoPath`: the latter normalizes separators
  // but not case, so on a case-insensitive filesystem `/work/GitPulse` and
  // `/WORK/gitpulse` are one checkout offered twice. `options` carries which
  // rule this host follows, and this is the only place that must obey it.
  for (const tab of openTabs) {
    const path = normalizeCheckout(tab.path);
    if (!path || !tabMatchesRegistered(path, repository.identity_key, options)) continue;
    const key = identityKey(path, options);
    if (!key || seen.has(key)) continue;
    seen.add(key);
    out.push({ path, label: tab.label || path, source: "open" });
  }
  const common = identityCommonDir(repository.identity_key);
  const derived = common ? checkoutBesideCommonDir(common) : null;
  if (derived) {
    const key = identityKey(derived, options);
    if (key && !seen.has(key)) out.push({ path: derived, label: derived, source: "derived" });
  }
  return out;
}

/**
 * `/work/repo/.git` -> `/work/repo`.
 *
 * Returns null for anything that is not a `.git` directory, which is exactly
 * the bare-repository case: there the common dir *is* the repository and
 * there is no working tree to hand an agent.
 */
export function checkoutBesideCommonDir(common: string): string | null {
  const normalized = normalizeCheckout(common);
  if (!normalized) return null;
  const trimmed = normalized.replace(/[\\/]+$/, "");
  const match = /^(.*)[\\/]\.git$/.exec(trimmed);
  const parent = match?.[1];
  return parent ? parent : null;
}

export function preferredCheckout(candidates: readonly CheckoutCandidate[]): string {
  return candidates[0]?.path ?? "";
}

export interface HandoffGate {
  ok: boolean;
  /** Why the launch button is disabled, in one sentence, or "" when it is not. */
  reason: string;
}

/**
 * Whether this handoff can be launched, and if not, the one thing to fix.
 *
 * Ordered by what the reader should do first, so the message changes as they
 * fill the form instead of naming the last problem alphabetically.
 */
export function handoffGate(input: {
  checkout: string;
  settings: HandoffSettings;
  acknowledgedBypass: boolean;
  dirty: boolean;
  busy: boolean;
}): HandoffGate {
  if (input.busy) return { ok: false, reason: "A launch is already in progress." };
  if (input.dirty) return { ok: false, reason: "Save your task edits before launching an agent." };
  if (!normalizeCheckout(input.checkout)) {
    return { ok: false, reason: "Choose the working checkout this agent should run in." };
  }
  if (input.settings.kind === "managed" && !supportsManaged(input.settings.provider)) {
    return { ok: false, reason: `${PROVIDER_LABELS[input.settings.provider]} supports terminal handoffs only.` };
  }
  if (input.settings.permission === "bypass" && !input.acknowledgedBypass) {
    return { ok: false, reason: "Bypass needs an explicit authorization for this attempt." };
  }
  return { ok: true, reason: "" };
}

/** Short, non-repeating state word for a run row. */
export function runStateLabel(state: string): string {
  switch (state) {
    case "prepared": return "Prepared";
    case "starting": return "Starting";
    case "running": return "Running";
    case "exited": return "Exited";
    case "failed": return "Failed to start";
    case "cancelled": return "Cancelled";
    case "unresolved": return "Unresolved";
    default: return state;
  }
}
