/**
 * The Claude API key Manvi uses when the Manvi pane picks Claude.
 *
 * The key is stored by the backend in the macOS Keychain. The webview can save
 * one, remove it and ask whether one exists; it can never read it back.
 */

import { invoke } from "../ipc/invoke";

/**
 * Why `raw` cannot be a key, or null when it can. Mirrors the backend's check
 * (`harness::claude_key::validate`), which stays the authority, so a paste of
 * the wrong thing is caught before a Keychain round trip.
 */
export function claudeKeyProblem(raw: string): string | null {
  const key = raw.trim();
  if (!key) return "Paste a key first.";
  if (!key.startsWith("sk-ant-")) return "That is not an Anthropic API key: keys start with sk-ant-.";
  if (key.length < 20 || key.length > 256) return "An Anthropic API key is between 20 and 256 characters.";
  if (!/^[A-Za-z0-9_-]+$/.test(key)) return "An Anthropic API key contains only letters, digits, - and _.";
  return null;
}

/** The `~/.anthropic-accounts` account a Claude-selected Manvi bills. */
export interface ClaudeAccount {
  /** The registry's name for the Console organisation. */
  name: string;
  /** That organisation's workspace id, when the registry records one. */
  workspace: string | null;
  /** Why it was chosen (`default` for the registry's first line). */
  source: string;
}

/** Where a Claude-selected Manvi gets its credential (`cmd_claude_key_status`). */
export interface ClaudeCredentialStatus {
  /** Whether GitPulse's own Keychain entry holds a key. */
  keySaved: boolean;
  /** The registry account; used instead of the saved key when present. */
  account: ClaudeAccount | null;
  /** A registry exists but could not be read: Manvi will not start on Claude. */
  accountError: string | null;
}

/** `claude-sonnet-5-5` → `Sonnet 5.5`; an id of another shape is returned as is. */
export function claudeModelLabel(id: string): string {
  const match = /^claude-([a-z]+)-(\d+)-(\d+)$/.exec(id);
  if (!match) return id;
  const [, family, major, minor] = match;
  return `${family[0].toUpperCase()}${family.slice(1)} ${major}.${minor}`;
}

/**
 * Why a Claude-selected Manvi cannot run on this credential answer, or null
 * when it can. Shared by the Manvi pill and the Settings card.
 */
export function claudeCredentialProblem(status: ClaudeCredentialStatus): string | null {
  if (status.accountError) return `~/.anthropic-accounts could not be read: ${status.accountError}`;
  if (status.account || status.keySaved) return null;
  return "No Claude credential: no ~/.anthropic-accounts registry and no key saved in Waylode.";
}

function text(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}

/** Narrows the backend's answer; anything malformed is an error, not "no key". */
export function claudeCredentialStatus(raw: unknown): ClaudeCredentialStatus {
  if (!raw || typeof raw !== "object") throw new Error("Unexpected Claude credential status from the backend.");
  const value = raw as Record<string, unknown>;
  if (typeof value.key_saved !== "boolean") throw new Error("Unexpected Claude credential status from the backend.");
  let account: ClaudeAccount | null = null;
  if (value.account != null) {
    const found = value.account as Record<string, unknown>;
    const name = text(found?.name);
    if (!name) throw new Error("The Anthropic account status named no account.");
    account = { name, workspace: text(found.workspace), source: text(found.source) ?? "default" };
  }
  return { keySaved: value.key_saved, account, accountError: text(value.account_error) };
}

export const claudeKey = {
  /** Which credential Manvi would use for Claude. */
  status: async () => claudeCredentialStatus(await invoke<unknown>("cmd_claude_key_status")),
  save: (key: string) => invoke<void>("cmd_claude_key_save", { key: key.trim() }),
  remove: () => invoke<void>("cmd_claude_key_delete"),
};
