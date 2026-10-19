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

export const claudeKey = {
  /** Whether a key is saved. */
  status: () => invoke<boolean>("cmd_claude_key_status"),
  save: (key: string) => invoke<void>("cmd_claude_key_save", { key: key.trim() }),
  remove: () => invoke<void>("cmd_claude_key_delete"),
};
