import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "svelte/compiler";
import {
  createSourceFile, forEachChild, isBinaryExpression, isCallExpression, isIdentifier,
  isNonNullExpression, isParenthesizedExpression, isPropertyAccessExpression,
  isRegularExpressionLiteral, ScriptTarget, SyntaxKind, type Node,
} from "typescript";
import { describe, expect, it } from "vitest";

/**
 * A backend error crosses IPC as `{ kind, message, detail? }`, and the one
 * place that reads it is `src/lib/ipc/invoke.ts`. Anything else that looks for
 * a phrase inside an error's text is a classifier that breaks silently when a
 * Rust message is reworded, or wrapped by a caller that prefixes it.
 *
 * This walks every non-test source file for a substring or regex match on a
 * value named like an error message, and fails on any that is not listed below
 * with the reason it is not an error classifier. The list can only shrink: an
 * entry that no longer matches anything fails too.
 */
const SRC = fileURLToPath(new URL("../src/", import.meta.url));
const GUARD = "lib/ipc/invoke.ts";

const METHODS = new Set(["includes", "startsWith", "endsWith", "indexOf", "match", "test", "search"]);
const MESSAGE_NAMED = /^(err|error|e|cause|reason|message|msg|detail|rejection|failure|text)$|(Error|Reason|Message|Msg|Failure)$/;
const PASS_THROUGH = new Set(["toLowerCase", "trim", "toString"]);

/** A match the scan finds that is not a classifier of a backend error. */
const NOT_A_CLASSIFIER: ReadonlyArray<{ file: string; needle: string; why: string }> = [
  { file: "lib/components/CommitRow.svelte", needle: "msg.match(", why: "parses a commit subject's conventional-commit prefix, which is data the user wrote" },
  { file: "lib/diagnostics/diagnostics.ts", needle: "effect_update_depth_exceeded", why: "recognises the webview's own console noise, not a backend error" },
  { file: "lib/diagnostics/diagnostics.ts", needle: "read and wrote the same state", why: "recognises the webview's own console noise, not a backend error" },
  { file: "lib/diagnostics/diagnostics.ts", needle: "[TAURI] Couldn't find callback id", why: "recognises the Tauri runtime's console noise" },
  { file: "lib/diagnostics/diagnostics.ts", needle: "IPC custom protocol failed", why: "recognises the Tauri runtime's console noise" },
  { file: "lib/diagnostics/diagnostics.ts", needle: "[hmr] Failed to reload", why: "recognises the dev server's console noise" },
  { file: "lib/diagnostics/diagnostics.ts", needle: "ResizeObserver loop", why: "recognises the browser's own console noise" },
  { file: "lib/palette/model.ts", needle: "text.endsWith(", why: "parses what the user typed in the palette" },
  { file: "lib/pulse/metrics.ts", needle: "message.indexOf(", why: "finds the end of a commit subject line" },
  { file: "lib/repos/gitPreflight.ts", needle: "/[\\s<>", why: "validates an identity the user typed; not an error" },
  { file: "lib/stores/mutationScan.ts", needle: "text.includes(name)", why: "searches source text for a symbol name" },
  { file: "lib/terminal/pasteGuard.ts", needle: "text.match(CONTROL)", why: "scans pasted text for control characters" },
  { file: "lib/workbench/taskQuery.ts", needle: "token.text.startsWith(", why: "parses the task query the user typed" },
  { file: "lib/workbench/taskQuickAdd.ts", needle: "text.startsWith(\"next-\")", why: "parses what the user typed in quick add" },
  { file: "lib/workbench/taskQuickAdd.ts", needle: "text.indexOf(", why: "parses what the user typed in quick add" },
];

/**
 * Backend text still read for a phrase, because the producer is a Rust report
 * struct or `gh`'s own output rather than a command error, so there is no kind
 * to carry yet. Each is a follow-up, named so it is not forgotten.
 */
const REMAINING_TEXT_READERS: ReadonlyArray<{ file: string; needle: string; why: string }> = [
  {
    file: "lib/async/deferral.ts",
    needle: "DEFERRED_UNDER_LOAD_MARKER",
    why: "text helper for already-flattened strings (toast severity, diagnostics severity, autoInit reports, githubAlerts report fields) that carry no kind; pinned to the Rust marker by deferral.test.ts. The command-error retry in invoke.ts no longer uses it",
  },
  {
    file: "lib/async/deferral.ts",
    needle: "RUN_TIMEOUT_MARKER",
    why: "outcomeUnknown reads the `create issue` outcome struct's error text (workbench/taskIssue.ts); that struct has no kind field yet",
  },
  {
    file: "lib/async/deferral.ts",
    needle: "SLOT_WAIT_SUFFIX",
    why: "the other half of outcomeUnknown, see the entry above",
  },
  {
    file: "lib/branches/deleteEscalation.ts",
    needle: "text.includes(",
    why: "reads git's own refusal wording for `branch -d` (unmerged, checked out elsewhere) plus two Rust guards; typing it means the backend classifying git stderr into branch-delete kinds, which no card has asked for yet",
  },
  {
    file: "lib/health/githubAlerts.ts",
    needle: "text.includes(",
    why: "reads `gh api`'s own wording to tell a disabled product from a failure; `unavailable_reason` on the report is the typed channel and this is its fallback for text the backend did not classify",
  },
  {
    file: "lib/ui/formatError.ts",
    needle: "file not found:",
    why: "reads the `File not found:` phrase of get_file_blob/get_file_content from a coverage reason string that the caller already flattened; needs a kind on that command's error",
  },
];

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walk(full));
    else if (/\.(ts|svelte)$/.test(entry.name) && !/\.(test|spec)\./.test(entry.name) && !entry.name.endsWith(".d.ts")) {
      out.push(full);
    }
  }
  return out;
}

function leafName(node: Node): string | null {
  if (isIdentifier(node)) return node.text;
  if (isPropertyAccessExpression(node)) return node.name.text;
  if (isNonNullExpression(node) || isParenthesizedExpression(node)) return leafName(node.expression);
  if (isCallExpression(node) && isPropertyAccessExpression(node.expression) && PASS_THROUGH.has(node.expression.name.text)) {
    return leafName(node.expression.expression);
  }
  if (isBinaryExpression(node) && node.operatorToken.kind === SyntaxKind.QuestionQuestionToken) return leafName(node.left);
  return null;
}

/** The substring/regex matches on a message-named value, as `file: call text`. */
export function messageMatches(file: string, source: string): string[] {
  let script = source;
  if (file.endsWith(".svelte")) {
    const component = parse(source, { modern: true, filename: file });
    script = [component.module, component.instance].flatMap((block) => {
      if (!block) return [];
      const content = block.content as unknown as { start: number; end: number };
      return [source.slice(content.start, content.end)];
    }).join("\n");
  }
  const parsed = createSourceFile(file, script, ScriptTarget.Latest, true);
  const found: string[] = [];
  const visit = (node: Node): void => {
    if (isCallExpression(node) && isPropertyAccessExpression(node.expression) && METHODS.has(node.expression.name.text)) {
      const method = node.expression.name.text;
      const receiver = leafName(node.expression.expression);
      const argument = node.arguments[0] ? leafName(node.arguments[0]) : null;
      const regexReceiver = isRegularExpressionLiteral(node.expression.expression);
      const onMessage = receiver !== null && MESSAGE_NAMED.test(receiver) && (method !== "test" || regexReceiver);
      const regexOnMessage = regexReceiver && method === "test" && argument !== null && MESSAGE_NAMED.test(argument);
      const matchOnMessage = method === "match" && argument !== null && MESSAGE_NAMED.test(argument);
      if (onMessage || regexOnMessage || matchOnMessage) found.push(node.getText(parsed).replace(/\s+/g, " "));
    }
    forEachChild(node, visit);
  };
  visit(parsed);
  return found;
}

describe("error-message classifier scan", () => {
  it("finds a classifier written the way this repo used to write them", () => {
    expect(messageMatches("x.ts", "const stale = (message: string) => /Refresh the stash list/i.test(message);"))
      .toEqual(["/Refresh the stash list/i.test(message)"]);
    expect(messageMatches("x.ts", "export const f = (error: string) => error.startsWith('Git does not know');"))
      .toEqual(["error.startsWith('Git does not know')"]);
    expect(messageMatches("x.svelte", `<script lang="ts">const f = (reason: string) => reason.includes("x");</script>`))
      .toEqual(['reason.includes("x")']);
  });

  it("leaves matches on values that are not message-named alone", () => {
    expect(messageMatches("x.ts", "const ok = names.includes(name) && /^[a-f0-9]+$/.test(oid);")).toEqual([]);
  });
});

describe("no error is classified by its text", () => {
  const allowed = [...NOT_A_CLASSIFIER, ...REMAINING_TEXT_READERS];
  const hits: Array<{ file: string; call: string }> = [];
  for (const full of walk(SRC)) {
    const file = path.relative(SRC, full).split(path.sep).join("/");
    if (file === GUARD) continue;
    for (const call of messageMatches(full, readFileSync(full, "utf8"))) hits.push({ file, call });
  }

  it("matches no message outside the one guard, except what is listed with its reason", () => {
    const unlisted = hits.filter((hit) => !allowed.some((entry) => entry.file === hit.file && hit.call.includes(entry.needle)));
    expect(unlisted.map((hit) => `${hit.file}: ${hit.call}`)).toEqual([]);
  });

  it("lists nothing that no longer matches", () => {
    const stale = allowed.filter((entry) => !hits.some((hit) => hit.file === entry.file && hit.call.includes(entry.needle)));
    expect(stale).toEqual([]);
  });

  it("gives every listed entry a real reason", () => {
    for (const entry of allowed) expect(entry.why.length, `${entry.file} ${entry.needle}`).toBeGreaterThan(20);
  });
});
