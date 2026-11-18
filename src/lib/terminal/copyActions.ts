/**
 * Pure terminal copy utilities: trailing-whitespace normalization,
 * drag-coalesced select-to-copy controller, write-only OSC 52 parsing, and
 * Markdown/agent-ready formatting.
 */

export const MAX_OSC52_BASE64_BYTES = 1_400_000;
export const MAX_AGENT_SELECTION_BYTES = 16 * 1024;

/**
 * Strips trailing spaces/tabs before line breaks and at the end of a terminal
 * selection while preserving leading indentation and interior spacing.
 */
export function normalizeSelectedText(raw: string): string {
  if (!raw) return "";
  return raw.replace(/[ \t]+(\r?\n)/g, "$1").replace(/[ \t]+$/, "");
}

export interface SelectToCopyDeps {
  enabled: () => boolean;
  hasSelection: () => boolean;
  getSelection: () => string;
  copy: (text: string) => Promise<boolean>;
  onCopied?: (text: string) => void;
}

export interface SelectToCopyController {
  onPointerDown(button: number): void;
  onPointerUp(): void;
  onSelectionChange(): void;
  reset(): void;
}

/**
 * Coalesces selection changes during a left-button drag so the clipboard is
 * written once on pointer release rather than on every dragged cell.
 * Outside a pointer drag (double/triple-click or programmatic select), copies
 * immediately when a non-whitespace selection is present.
 */
export function createSelectToCopyController(
  deps: SelectToCopyDeps,
): SelectToCopyController {
  let pointerSelecting = false;
  let pending = false;
  let lastCopied: string | null = null;

  function flush() {
    pending = false;
    if (!deps.enabled()) return;
    if (!deps.hasSelection()) {
      lastCopied = null;
      return;
    }
    const normalized = normalizeSelectedText(deps.getSelection());
    if (normalized.trim().length === 0) return;
    if (normalized === lastCopied) return;
    lastCopied = normalized;
    void Promise.resolve(deps.copy(normalized))
      .then((ok) => {
        if (ok) {
          deps.onCopied?.(normalized);
        } else if (lastCopied === normalized) {
          lastCopied = null;
        }
      })
      .catch(() => {
        if (lastCopied === normalized) lastCopied = null;
      });
  }

  return {
    onPointerDown(button: number) {
      if (button === 0) {
        pointerSelecting = true;
      }
    },
    onPointerUp() {
      if (!pointerSelecting) return;
      pointerSelecting = false;
      if (pending) flush();
    },
    onSelectionChange() {
      if (!deps.hasSelection()) {
        lastCopied = null;
        pending = false;
        return;
      }
      if (!deps.enabled()) return;
      if (pointerSelecting) {
        pending = true;
        return;
      }
      flush();
    },
    reset() {
      pointerSelecting = false;
      pending = false;
      lastCopied = null;
    },
  };
}

/**
 * Parses a write-only OSC 52 clipboard payload (`<targets>;<base64>`).
 *
 * Security invariants:
 * - Clipboard read queries (`?`) always return `null` and are never answered.
 * - Base64 payloads are capped at `MAX_OSC52_BASE64_BYTES` (~1 MiB UTF-8).
 * - Invalid UTF-8 and embedded NUL bytes are rejected.
 */
export function parseOsc52(payload: string): string | null {
  if (!payload || payload.length > MAX_OSC52_BASE64_BYTES + 32) return null;
  const sep = payload.indexOf(";");
  if (sep < 0) return null;

  const targets = payload.slice(0, sep).trim();
  if (targets && !/^[cps0-7]+$/.test(targets)) return null;

  const rawB64 = payload.slice(sep + 1).replace(/[\r\n\t ]+/g, "");
  if (!rawB64 || rawB64 === "?" || rawB64.length > MAX_OSC52_BASE64_BYTES) {
    return null;
  }
  if (!/^[A-Za-z0-9+/]+={0,2}$/.test(rawB64)) return null;
  if (rawB64.includes("=") && !/=+$/.test(rawB64)) return null;

  const unpaddedLen = rawB64.replace(/=+$/, "").length;
  if (unpaddedLen % 4 === 1) return null;
  const padded =
    rawB64.length % 4 === 0
      ? rawB64
      : rawB64 + "=".repeat(4 - (rawB64.length % 4));

  try {
    const binary = atob(padded);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    const decoded = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    if (!decoded || decoded.includes("\0")) return null;
    return decoded;
  } catch {
    return null;
  }
}

function chooseFence(body: string): string {
  let longest = 2;
  for (const match of body.matchAll(/`{3,}/g)) {
    if (match[0].length > longest) longest = match[0].length;
  }
  return "`".repeat(longest + 1);
}

/**
 * Formats terminal output (with optional command, cwd, and exit code metadata)
 * as a fenced Markdown block ready to paste into issues, PRs, or chat.
 */
export function formatTerminalMarkdownBlock(opts: {
  text: string;
  command?: string | null;
  cwd?: string | null;
  exitCode?: number | null;
}): string {
  const lines: string[] = [];
  if (opts.cwd && opts.cwd.trim()) {
    lines.push(`# cwd: ${opts.cwd.trim()}`);
  }
  if (opts.command && opts.command.trim()) {
    lines.push(`$ ${opts.command.trim()}`);
  }
  if (opts.exitCode !== null && opts.exitCode !== undefined) {
    lines.push(`# exit ${opts.exitCode}`);
  }
  const cleanText = normalizeSelectedText(opts.text).replace(/\n+$/, "");
  if (cleanText) {
    lines.push(cleanText);
  }
  const body = lines.join("\n");
  const fence = chooseFence(body);
  return `${fence}text\n${body}\n${fence}`;
}

/**
 * Formats selected terminal text for handing to a coding agent session,
 * bounded to `MAX_AGENT_SELECTION_BYTES`.
 */
export function formatSelectionForAgent(opts: {
  text: string;
  cwd?: string | null;
}): string {
  const cleanText = normalizeSelectedText(opts.text).replace(/\n+$/, "");
  const bounded =
    cleanText.length > MAX_AGENT_SELECTION_BYTES
      ? `${cleanText.slice(0, MAX_AGENT_SELECTION_BYTES)}\n… [truncated]`
      : cleanText;
  const cwdPart =
    opts.cwd && opts.cwd.trim() ? ` (in \`${opts.cwd.trim()}\`)` : "";
  const fence = chooseFence(bounded);
  return `Terminal output${cwdPart}:\n${fence}text\n${bounded}\n${fence}`;
}
