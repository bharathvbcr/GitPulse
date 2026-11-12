/**
 * Shell and agent OSC telemetry (`OSC 7`, `OSC 133`, `OSC 7501`), loopback
 * dev-server readiness detection, and semantic command-block tracking for
 * prompt navigation (`⌘↑` / `⌘↓`) and "Copy / Open last command output".
 */

import { clean } from "./sessionActivity";

export const MAX_COMMAND_BLOCKS = 64;
export const MAX_BLOCK_OUTPUT_BYTES = 256 * 1024;

export interface BufferMarker {
  readonly line: number;
  readonly isDisposed?: boolean;
  dispose(): void;
}

export type ShellMark =
  | { kind: "prompt_start" }
  | { kind: "command_start" }
  | { kind: "command_executed" }
  | { kind: "command_finished"; exitCode: number | null };

export type ShellMarkEvent = ShellMark;

export type HerdrState = "working" | "blocked" | "idle" | "done" | "error";

export type HerdrAgentState = HerdrState;

export interface AgentOscStatus {
  state: HerdrAgentState | "clear";
  detail: string | null;
}

export interface CommandBlockSnapshot {
  promptLine: number;
  outputStartLine: number | null;
  outputEndLine: number | null;
  exitCode: number | null;
  running: boolean;
}

function normalizePosixSegments(path: string): string[] | null {
  const out: string[] = [];
  for (const part of path.replace(/\\/g, "/").split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (out.length === 0) return null;
      out.pop();
      continue;
    }
    out.push(part);
  }
  return out;
}

function windowsDriveOf(path: string): string | null {
  if (path.length >= 3 && path[1] === ":" && /[A-Za-z]/.test(path[0]) && (path[2] === "/" || path[2] === "\\")) {
    return path[0].toLowerCase();
  }
  return null;
}

/**
 * Decodes an `OSC 7` working-directory payload (`file://host/path` or `/path`)
 * into a clean absolute path, or `null` when malformed or containing control bytes.
 */
export function parseOsc7Path(payload: string): string | null {
  if (!payload || payload.length > 4096) return null;
  const trimmed = payload.trim();
  if (!trimmed) return null;

  let encodedPath = trimmed;
  if (trimmed.toLowerCase().startsWith("file://")) {
    const rest = trimmed.slice("file://".length);
    const slash = rest.indexOf("/");
    if (slash < 0) return null;
    encodedPath = rest.slice(slash);
  } else if (!trimmed.startsWith("/") && !windowsDriveOf(trimmed)) {
    return null;
  }

  let decoded: string;
  try {
    decoded = decodeURIComponent(encodedPath);
  } catch {
    return null;
  }

  // Strip leading slash on Windows `file:///C:/repo` URIs (`"/C:/repo"` -> `"C:/repo"`).
  if (decoded.length >= 4 && decoded[0] === "/" && windowsDriveOf(decoded.slice(1))) {
    decoded = decoded.slice(1);
  }

  for (let i = 0; i < decoded.length; i++) {
    const code = decoded.charCodeAt(i);
    if (code < 0x20 || code === 0x7f) return null;
  }

  while (decoded.length > 1 && (decoded.endsWith("/") || decoded.endsWith("\\"))) {
    if (decoded.length === 3 && windowsDriveOf(decoded)) break;
    decoded = decoded.slice(0, -1);
  }
  return decoded || null;
}

/**
 * Resolves an `OSC 7` absolute directory against `repoPath`.
 *
 * Returns `""` for the repository root itself, a POSIX relative subdirectory
 * (`"packages/ui"`) when strictly inside `repoPath`, or `null` when outside
 * `repoPath` (including sibling prefixes like `/repo-evil`).
 */
export function resolveOsc7Subdir(repoPath: string, osc7Path: string): string | null {
  if (!repoPath || !osc7Path) return null;
  if (windowsDriveOf(repoPath) !== windowsDriveOf(osc7Path)) return null;
  const rootSegs = normalizePosixSegments(repoPath);
  const cwdSegs = normalizePosixSegments(osc7Path);
  if (!rootSegs || !cwdSegs) return null;
  if (cwdSegs.length < rootSegs.length) return null;
  for (let i = 0; i < rootSegs.length; i++) {
    if (cwdSegs[i] !== rootSegs[i]) return null;
  }
  return cwdSegs.slice(rootSegs.length).join("/");
}

/**
 * Decodes FinalTerm / iTerm2 / VS Code semantic prompt marks (`OSC 133 ; ...`).
 */
export function parseOsc133(payload: string): ShellMarkEvent | null {
  if (!payload || payload.length > 256) return null;
  const parts = payload.trim().split(";");
  const tag = parts[0]?.trim();
  switch (tag) {
    case "A":
      return { kind: "prompt_start" };
    case "B":
      return { kind: "command_start" };
    case "C":
      return { kind: "command_executed" };
    case "D": {
      const rawCode = parts[1]?.trim();
      if (!rawCode) return { kind: "command_finished", exitCode: null };
      if (!/^-?\d{1,6}$/.test(rawCode)) return { kind: "command_finished", exitCode: null };
      const parsed = Number.parseInt(rawCode, 10);
      return { kind: "command_finished", exitCode: Number.isFinite(parsed) ? parsed : null };
    }
    default:
      return null;
  }
}

function normalizeHerdrToken(token: string): HerdrAgentState | "clear" | null {
  switch (token.trim().toLowerCase()) {
    case "working":
    case "running":
      return "working";
    case "blocked":
    case "waiting":
    case "needs_input":
      return "blocked";
    case "idle":
      return "idle";
    case "done":
    case "completed":
      return "done";
    case "error":
    case "failed":
      return "error";
    case "clear":
    case "reset":
      return "clear";
    default:
      return null;
  }
}

function decodeBase64Utf8(raw: string): string | null {
  const trimmed = raw.trim();
  if (!trimmed || trimmed.length > 2048) return null;
  const normalized = trimmed.replace(/-/g, "+").replace(/_/g, "/");
  const remainder = normalized.length % 4;
  if (remainder === 1) return null;
  const padded =
    remainder === 0 ? normalized : normalized + "=".repeat(4 - remainder);
  try {
    const binary = atob(padded);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) {
      bytes[i] = binary.charCodeAt(i);
    }
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

function extractHerdrKvDetail(fields: readonly string[]): string | null {
  for (const field of fields) {
    const eq = field.indexOf("=");
    if (eq <= 0) continue;
    const key = field.slice(0, eq).trim().toLowerCase();
    const val = field.slice(eq + 1).trim();
    if (
      key === "detail_b64" ||
      key === "message_b64" ||
      key === "msg_b64" ||
      key === "reason_b64"
    ) {
      const decoded = decodeBase64Utf8(val);
      if (decoded !== null) return decoded ? clean(decoded, 160) : null;
    } else if (key === "detail" || key === "message" || key === "msg" || key === "reason") {
      return val ? clean(val, 160) : null;
    }
  }
  return null;
}

/**
 * Decodes Herdr terminal agent coordination sequences (`OSC 7501 ; [status;]<state> [; <detail>]`,
 * colon-separated `OSC 7501 ; status:<state>:message_b64=<b64>`, `OSC 7501 ; clear`,
 * or `OSC 7501 ; state=<state>;message=<detail>`). Ignores discovery probes (`?` / `probe`).
 */
export function parseOsc7501(payload: string): AgentOscStatus | null {
  if (!payload || payload.length > 2048) return null;
  const trimmed = payload.trim();
  if (!trimmed) return null;
  const lower = trimmed.toLowerCase();
  if (trimmed === "?" || lower === "probe") return null;
  if (lower === "clear" || lower === "reset") {
    return { state: "clear", detail: null };
  }

  const sep = trimmed.includes(";") ? ";" : trimmed.includes(":") ? ":" : null;
  const parts = sep ? trimmed.split(sep) : [trimmed];
  let idx = 0;
  if (
    parts.length > 1 &&
    !parts[0].includes("=") &&
    parts[0].trim().toLowerCase() === "status"
  ) {
    idx = 1;
  }

  const head = parts[idx]?.trim() ?? "";
  if (!head) return null;

  if (!head.includes("=")) {
    const state = normalizeHerdrToken(head);
    if (!state) return null;
    if (state === "clear") return { state: "clear", detail: null };
    const rest = parts.slice(idx + 1);
    if (rest.length === 0) return { state, detail: null };
    if (rest.every((f) => /^[A-Za-z_][A-Za-z0-9_]*=/.test(f.trim()))) {
      return { state, detail: extractHerdrKvDetail(rest) };
    }
    const rawDetail = rest.join(sep ?? ";").trim();
    return {
      state,
      detail: rawDetail ? clean(rawDetail, 160) : null,
    };
  }

  const slice = parts.slice(idx);
  let state: HerdrAgentState | "clear" | null = null;
  for (const field of slice) {
    const eq = field.indexOf("=");
    if (eq <= 0) continue;
    const key = field.slice(0, eq).trim().toLowerCase();
    const val = field.slice(eq + 1).trim();
    if (key === "state" || key === "status") {
      state = normalizeHerdrToken(val);
    }
  }
  if (!state) return null;
  if (state === "clear") return { state: "clear", detail: null };
  return { state, detail: extractHerdrKvDetail(slice) };
}

const ANSI_ESCAPE_RE = /\x1b(?:\][^\x07\x1b]*(?:\x07|\x1b\\)|\[[0-?]*[ -/]*[@-~]|[@-_])/g;
const LOOPBACK_URL_RE =
  /https?:\/\/(?:localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\]):(\d{1,5})(?:\/[^\s"'`<>|)\\]*)?/i;

/**
 * Detects a loopback HTTP(S) server readiness URL in a chunk of terminal output
 * (`http://localhost:5173/`, `http://0.0.0.0:3000`, `http://127.0.0.1:8080/app`),
 * normalizing `0.0.0.0` to `localhost` so a browser view can connect directly.
 */
export function extractLoopbackServerUrl(chunk: string): string | null {
  if (!chunk) return null;
  const bounded = chunk.length > 8192 ? chunk.slice(0, 8192) : chunk;
  if (!bounded.includes("http://") && !bounded.includes("https://") && !bounded.includes("\x1b")) {
    return null;
  }
  const stripped = bounded.replace(ANSI_ESCAPE_RE, "");
  const match = LOOPBACK_URL_RE.exec(stripped);
  if (!match) return null;
  const port = Number.parseInt(match[1], 10);
  if (!Number.isFinite(port) || port < 1 || port > 65535) return null;
  let candidate = match[0].replace(/[.,;:!?)'"\]]+$/, "");
  try {
    const parsed = new URL(candidate);
    if (parsed.hostname === "0.0.0.0") {
      parsed.hostname = "localhost";
      candidate = parsed.toString();
    }
    return candidate;
  } catch {
    return null;
  }
}

interface InternalBlock {
  promptMarker: BufferMarker;
  outputStartMarker: BufferMarker | null;
  outputEndMarker: BufferMarker | null;
  startedAtMs: number | null;
  durationMs: number | null;
  exitCode: number | null;
  running: boolean;
}

export interface CompletedCommandDetail {
  promptMarker: BufferMarker;
  outputStartMarker: BufferMarker | null;
  outputEndMarker: BufferMarker | null;
  exitCode: number | null;
  durationMs: number | null;
}

function markerAlive(marker: BufferMarker | null): marker is BufferMarker {
  return Boolean(marker && !marker.isDisposed && marker.line >= 0);
}

function disposeBlock(block: InternalBlock): void {
  block.promptMarker.dispose();
  block.outputStartMarker?.dispose();
  block.outputEndMarker?.dispose();
}

export interface CommandBlockTracker {
  onMark(
    event: ShellMarkEvent,
    createMarker: () => BufferMarker | undefined,
    nowMs?: number,
  ): void;
  blocks(): CommandBlockSnapshot[];
  jumpTarget(currentViewportY: number, direction: "prev" | "next"): number | null;
  lastCompletedBlock(): CommandBlockSnapshot | null;
  lastFinishedDetail(): CompletedCommandDetail | null;
  clear(): void;
}

/**
 * Tracks up to `maxBlocks` semantic command blocks using xterm `IMarker` anchors,
 * which automatically track scrollback ring trims and report `line === -1` once
 * scrolled off the top of scrollback.
 */
export function createCommandBlockTracker(maxBlocks = MAX_COMMAND_BLOCKS): CommandBlockTracker {
  const cap = Math.max(1, Math.floor(maxBlocks));
  const items: InternalBlock[] = [];
  let latestFinished: InternalBlock | null = null;

  function pruneDisposed(): void {
    for (let i = items.length - 1; i >= 0; i--) {
      if (!markerAlive(items[i].promptMarker)) {
        if (latestFinished === items[i]) latestFinished = null;
        disposeBlock(items[i]);
        items.splice(i, 1);
      }
    }
  }

  function pushPrompt(marker: BufferMarker): InternalBlock {
    pruneDisposed();
    const block: InternalBlock = {
      promptMarker: marker,
      outputStartMarker: null,
      outputEndMarker: null,
      startedAtMs: null,
      durationMs: null,
      exitCode: null,
      running: false,
    };
    items.push(block);
    while (items.length > cap) {
      const oldest = items.shift();
      if (oldest) {
        if (latestFinished === oldest) latestFinished = null;
        disposeBlock(oldest);
      }
    }
    return block;
  }

  return {
    onMark(event, createMarker, nowMs = Date.now()) {
      pruneDisposed();
      if (event.kind === "prompt_start") {
        const current = items[items.length - 1];
        // Close an unclosed running command if the shell went straight to next prompt.
        if (current && current.running) {
          current.running = false;
          if (!current.outputEndMarker) {
            const endMarker = createMarker();
            if (endMarker) current.outputEndMarker = endMarker;
          }
        }
        const marker = createMarker();
        if (!marker) return;
        // Deduplicate back-to-back `133;A` on the exact same line.
        if (current && !current.outputStartMarker && current.promptMarker.line === marker.line) {
          marker.dispose();
          return;
        }
        pushPrompt(marker);
        return;
      }

      if (event.kind === "command_start") {
        if (items.length === 0) {
          const marker = createMarker();
          if (marker) pushPrompt(marker);
        }
        return;
      }

      if (event.kind === "command_executed") {
        let target = items[items.length - 1];
        const marker = createMarker();
        if (!marker) return;
        if (!target || target.outputEndMarker !== null) {
          const promptCopy = createMarker();
          if (!promptCopy) {
            marker.dispose();
            return;
          }
          target = pushPrompt(promptCopy);
        }
        target.outputStartMarker?.dispose();
        target.outputStartMarker = marker;
        target.startedAtMs = Number.isFinite(nowMs) ? nowMs : null;
        target.durationMs = null;
        target.running = true;
        return;
      }

      if (event.kind === "command_finished") {
        const target = items[items.length - 1];
        if (!target) return;
        target.running = false;
        target.exitCode = event.exitCode;
        if (target.startedAtMs !== null && Number.isFinite(nowMs) && nowMs >= target.startedAtMs) {
          target.durationMs = Math.round(nowMs - target.startedAtMs);
        }
        target.outputEndMarker?.dispose();
        target.outputEndMarker = createMarker() ?? null;
        latestFinished = target;
      }
    },

    blocks() {
      pruneDisposed();
      return items.map((item) => ({
        promptLine: item.promptMarker.line,
        outputStartLine: markerAlive(item.outputStartMarker) ? item.outputStartMarker.line : null,
        outputEndLine: markerAlive(item.outputEndMarker) ? item.outputEndMarker.line : null,
        exitCode: item.exitCode,
        running: item.running,
      }));
    },

    jumpTarget(currentViewportY, direction) {
      pruneDisposed();
      if (items.length === 0) return null;
      const lines: number[] = [];
      for (const item of items) {
        const y = item.promptMarker.line;
        if (y >= 0 && (lines.length === 0 || lines[lines.length - 1] !== y)) {
          lines.push(y);
        }
      }
      if (lines.length === 0) return null;
      if (direction === "prev") {
        for (let i = lines.length - 1; i >= 0; i--) {
          if (lines[i] < currentViewportY) return lines[i];
        }
        return lines[0];
      }
      for (let i = 0; i < lines.length; i++) {
        if (lines[i] > currentViewportY) return lines[i];
      }
      return lines[lines.length - 1];
    },

    lastCompletedBlock() {
      pruneDisposed();
      for (let i = items.length - 1; i >= 0; i--) {
        const item = items[i];
        if (
          !item.running &&
          markerAlive(item.outputStartMarker) &&
          markerAlive(item.outputEndMarker)
        ) {
          return {
            promptLine: item.promptMarker.line,
            outputStartLine: item.outputStartMarker.line,
            outputEndLine: item.outputEndMarker.line,
            exitCode: item.exitCode,
            running: false,
          };
        }
      }
      return null;
    },

    lastFinishedDetail() {
      pruneDisposed();
      if (!latestFinished || !markerAlive(latestFinished.promptMarker)) return null;
      return {
        promptMarker: latestFinished.promptMarker,
        outputStartMarker: markerAlive(latestFinished.outputStartMarker)
          ? latestFinished.outputStartMarker
          : null,
        outputEndMarker: markerAlive(latestFinished.outputEndMarker)
          ? latestFinished.outputEndMarker
          : null,
        exitCode: latestFinished.exitCode,
        durationMs: latestFinished.durationMs,
      };
    },

    clear() {
      latestFinished = null;
      while (items.length > 0) {
        const item = items.pop();
        if (item) disposeBlock(item);
      }
    },
  };
}

export interface ReadableTerminalBuffer {
  readonly length: number;
  getLine(y: number):
    | {
        readonly isWrapped?: boolean;
        translateToString(trimRight?: boolean): string;
      }
    | undefined;
}

/**
 * Extracts plain-text output between `block.outputStartLine` and `block.outputEndLine`,
 * joining soft-wrapped rows without spurious newlines and bounding size at `maxBytes`.
 */
export function extractBlockOutput(
  buffer: ReadableTerminalBuffer,
  block: Pick<CommandBlockSnapshot, "outputStartLine" | "outputEndLine">,
  maxBytes = MAX_BLOCK_OUTPUT_BYTES,
): string {
  if (block.outputStartLine === null || block.outputEndLine === null) return "";
  // Output begins right after the line where `133;C` was emitted (the command input line)
  // unless `outputStartLine === outputEndLine`, in which case check that single line.
  const start = Math.max(
    0,
    block.outputEndLine > block.outputStartLine ? block.outputStartLine + 1 : block.outputStartLine,
  );
  const end = Math.min(buffer.length, block.outputEndLine);
  if (end <= start) return "";

  const lines: string[] = [];
  let totalChars = 0;
  for (let y = start; y < end; y++) {
    const row = buffer.getLine(y);
    if (!row) continue;
    const nextRow = y + 1 < end ? buffer.getLine(y + 1) : undefined;
    const continuesOnNext = Boolean(nextRow?.isWrapped);
    const text = row.translateToString(!continuesOnNext);
    if (row.isWrapped && lines.length > 0) {
      lines[lines.length - 1] += text;
    } else {
      lines.push(text);
    }
    totalChars += text.length + 1;
    if (totalChars >= maxBytes) break;
  }

  let joined = lines.join("\n");
  if (joined.length > maxBytes) {
    joined = joined.slice(0, maxBytes);
  }
  return joined.replace(/\n+$/, "");
}

const MAX_COMMAND_LINE_CHARS = 2048;
const MAX_FAILED_COMMAND_AGENT_BYTES = 12 * 1024;
const SHELL_PROMPT_PREFIX_RE = /^.*?(?:[%$#❯➜›>])\s+/;

export function formatCommandDuration(durationMs: number): string {
  if (!Number.isFinite(durationMs) || durationMs < 0) return "0ms";
  if (durationMs < 1000) return `${Math.round(durationMs)}ms`;
  if (durationMs < 60_000) return `${(durationMs / 1000).toFixed(1)}s`;
  const mins = Math.floor(durationMs / 60_000);
  const secs = Math.round((durationMs % 60_000) / 1000);
  return `${mins}m ${secs}s`;
}

/**
 * Extracts the executed command string from the prompt block (`promptLine..outputStartLine`),
 * stripping standard shell prompt prefixes when present.
 */
export function extractBlockCommand(
  buffer: ReadableTerminalBuffer,
  block: { promptLine: number; outputStartLine: number | null },
): string {
  if (block.promptLine < 0 || block.promptLine >= buffer.length) return "";
  const end = Math.min(
    buffer.length - 1,
    block.outputStartLine !== null && block.outputStartLine >= block.promptLine
      ? block.outputStartLine
      : block.promptLine,
  );
  const parts: string[] = [];
  for (let y = block.promptLine; y <= end; y++) {
    const row = buffer.getLine(y);
    if (!row) continue;
    const nextRow = y + 1 <= end ? buffer.getLine(y + 1) : undefined;
    const text = row.translateToString(!nextRow?.isWrapped);
    if (row.isWrapped && parts.length > 0) {
      parts[parts.length - 1] += text;
    } else {
      parts.push(text);
    }
    if (parts.join("\n").length >= MAX_COMMAND_LINE_CHARS) break;
  }
  if (parts.length === 0) return "";
  parts[0] = parts[0].replace(SHELL_PROMPT_PREFIX_RE, "");
  return parts.join("\n").trim().slice(0, MAX_COMMAND_LINE_CHARS);
}

export interface FailedCommandAgentContext {
  command: string;
  exitCode: number | null;
  durationMs: number | null;
  cwd?: string | null;
  output: string;
}

export function formatFailedCommandForAgent(ctx: FailedCommandAgentContext): string {
  const statusPart = ctx.exitCode !== null ? `exit ${ctx.exitCode}` : "failed";
  const durationPart =
    ctx.durationMs !== null && Number.isFinite(ctx.durationMs)
      ? `, ${formatCommandDuration(ctx.durationMs)}`
      : "";
  const cwdPart = ctx.cwd && ctx.cwd.trim() ? ` in \`${ctx.cwd.trim()}\`` : "";
  const cmdLine = ctx.command.trim() ? `$ ${ctx.command.trim()}\n` : "";
  const trimmedOut =
    ctx.output.length > MAX_FAILED_COMMAND_AGENT_BYTES
      ? `${ctx.output.slice(0, MAX_FAILED_COMMAND_AGENT_BYTES)}\n… [truncated]`
      : ctx.output;
  const body = `${cmdLine}${trimmedOut}`.trimEnd();
  return `Command failed (${statusPart}${durationPart})${cwdPart}:\n\`\`\`text\n${body}\n\`\`\``;
}

export interface DisposableDecorationEntry {
  marker: { readonly isDisposed: boolean; dispose(): void };
  decoration: { readonly isDisposed: boolean; dispose(): void };
}

export interface CommandDecorationRing {
  push(entry: DisposableDecorationEntry): void;
  size(): number;
  clear(): void;
}

const DEFAULT_MAX_DECORATIONS = 64;

/**
 * Bounded ring of xterm `IDecoration` + `IMarker` pairs so long-running interactive
 * shells never accumulate unbounded DOM/decoration listeners.
 */
export function createCommandDecorationRing(
  maxDecorations = DEFAULT_MAX_DECORATIONS,
): CommandDecorationRing {
  const cap = Math.max(1, Math.floor(maxDecorations));
  const entries: DisposableDecorationEntry[] = [];

  function disposeEntry(entry: DisposableDecorationEntry): void {
    try {
      if (!entry.decoration.isDisposed) entry.decoration.dispose();
    } catch {
      /* ignore disposed xterm decoration */
    }
    try {
      if (!entry.marker.isDisposed) entry.marker.dispose();
    } catch {
      /* ignore disposed xterm marker */
    }
  }

  function pruneDisposed(): void {
    for (let i = entries.length - 1; i >= 0; i--) {
      if (entries[i].decoration.isDisposed || entries[i].marker.isDisposed) {
        disposeEntry(entries[i]);
        entries.splice(i, 1);
      }
    }
  }

  return {
    push(entry) {
      pruneDisposed();
      entries.push(entry);
      while (entries.length > cap) {
        const oldest = entries.shift();
        if (oldest) disposeEntry(oldest);
      }
    },
    size() {
      pruneDisposed();
      return entries.length;
    },
    clear() {
      while (entries.length > 0) {
        const entry = entries.pop();
        if (entry) disposeEntry(entry);
      }
    },
  };
}
