import { formatCoveragePercent } from "./format";
import { coverageCommandsAreCumulative } from "./scripts";
import type {
  CoverageArtifact,
  CoverageFamilyStatus,
  CoverageLanguageSplit,
  CoverageReport,
  FileCoverageSummary,
} from "./types";

const MAX_FILES_LISTED = 30;
const MAX_ARTIFACTS_LISTED = 40;
const MAX_ISSUE_BODY_BYTES = 60 * 1024;
const ISSUE_CLIP_NOTE = "\n\n[GitPulse clipped this draft to stay below GitHub's body limit.]";

export interface CoverageIssueDraft {
  title: string;
  body: string;
  clipped: boolean;
}

/**
 * Coverage numbers cross the IPC boundary as plain JSON, so a buggy or
 * hostile producer can hand over NaN, Infinity, negatives or missing fields.
 * Everything is re-validated here: the renderer must hand the model (and the
 * clipboard) a well-formed document and must never throw on any input shape.
 */
function safePercent(value: unknown): string {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return "0.0%";
  return formatCoveragePercent(Math.min(100, value));
}

function safeCount(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return 0;
  return Math.min(Number.MAX_SAFE_INTEGER, Math.max(0, Math.trunc(value)));
}

function safeText(value: unknown): string {
  if (typeof value !== "string") return "";
  // Paths and producer-owned labels are rendered as one report line. Keep
  // embedded controls visible instead of letting a crafted filename inject a
  // new heading, runnable command, or terminal control into the model prompt.
  return Array.from(value, (char) => {
    const code = char.codePointAt(0) ?? 0;
    if (char === "\n") return "\\n";
    if (char === "\r") return "\\r";
    if (char === "\t") return "\\t";
    if ([0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069].includes(code)) {
      return `\\u{${code.toString(16)}}`;
    }
    if (code < 0x20 || code === 0x7f) return `\\u{${code.toString(16).padStart(4, "0")}}`;
    return char;
  }).join("");
}

function safeList<T>(value: T[] | undefined): T[] {
  return Array.isArray(value) ? value : [];
}

/**
 * Deterministic plain-text rendering of a coverage report, used both as the
 * payload for `cmd_ai_coverage_report` and for copy-to-clipboard. Section
 * order is stable so prompts and tests stay comparable across runs; capped
 * lists carry their counters so "shorter list" never reads as "everything".
 */
export function formatCoverageReport(report: CoverageReport, repoPath: string): string {
  const data = (report ?? {}) as Partial<CoverageReport>;
  const languageRows = safeList(data.languages);
  const fileRows = safeList(data.files);
  const artifactRows = safeList(data.artifacts);
  const familyRows = safeList(data.families);

  const out: string[] = [];
  out.push(`Coverage report — ${safeText(repoPath)}`);

  let overall =
    `${safePercent(data.overall?.percentage)} ` +
    `(${safeCount(data.overall?.lines_hit)}/${safeCount(data.overall?.lines_found)} lines)`;
  // A capped scan must say so in the same breath as its totals.
  if (data.truncated === true) overall += " [SCAN TRUNCATED — results partial]";
  out.push("", "OVERALL", overall);

  if (languageRows.length > 0) {
    out.push("", "PER-LANGUAGE");
    for (const lang of languageRows as CoverageLanguageSplit[]) {
      if (!lang || typeof lang !== "object") continue;
      out.push(
        `${safeText(lang.language)}: ${safePercent(lang.percentage)} ` +
          `(${safeCount(lang.lines_hit)}/${safeCount(lang.lines_found)} lines, ${safeCount(lang.files)} files)`,
      );
    }
  }

  // report.files already arrives sorted worst-first; slicing preserves that.
  if (fileRows.length > 0) {
    out.push(
      "",
      `LOWEST-COVERED FILES (worst first, showing ${Math.min(fileRows.length, MAX_FILES_LISTED)} of ${fileRows.length})`,
    );
    for (const file of fileRows.slice(0, MAX_FILES_LISTED) as FileCoverageSummary[]) {
      if (!file || typeof file !== "object") continue;
      out.push(
        `- ${safeText(file.path)}: ${safePercent(file.percentage)} ` +
          `(${safeCount(file.lines_hit)}/${safeCount(file.lines_found)} lines)`,
      );
    }
  }

  if (artifactRows.length > 0) {
    out.push(
      "",
      `ARTIFACTS (showing ${Math.min(artifactRows.length, MAX_ARTIFACTS_LISTED)} of ${artifactRows.length})`,
    );
    for (const artifact of artifactRows.slice(0, MAX_ARTIFACTS_LISTED) as CoverageArtifact[]) {
      if (!artifact || typeof artifact !== "object") continue;
      let line = `- ${safeText(artifact.path)} (${safeText(artifact.format)})`;
      if (artifact.skipped === true) {
        const reason = safeText(artifact.skip_reason);
        line += reason ? ` — skipped: ${reason}` : " — skipped";
      }
      out.push(line);
    }
    if (artifactRows.length > MAX_ARTIFACTS_LISTED) {
      out.push(`…and ${artifactRows.length - MAX_ARTIFACTS_LISTED} more`);
    }
  }

  // Naming what was looked for but not found steers the model toward fixing
  // the missing data instead of inventing numbers for it.
  const missingFamilies = (
    familyRows as CoverageFamilyStatus[]
  ).filter((family) => !!family && typeof family === "object" && family.found !== true);
  if (missingFamilies.length > 0) {
    out.push("", "FAMILIES WITHOUT REPORT");
    for (const family of missingFamilies) {
      const formats = Array.isArray(family.expected_formats)
        ? family.expected_formats.map(safeText).filter(Boolean).join(", ")
        : "";
      const commands = Array.isArray(family.suggested_commands)
        ? family.suggested_commands.map(safeText).filter(Boolean)
        : [];
      const setup = Array.isArray(family.setup_commands)
        ? family.setup_commands.map(safeText).filter(Boolean)
        : [];
      const toolReady = family.tool_ready !== false;
      const toolDetail = safeText(family.tool_detail);
      const duration = safeText(family.duration_hint);
      const parts: string[] = [];
      if (!toolReady && toolDetail) parts.push(toolDetail);
      if (!toolReady && setup.length > 0) {
        parts.push(`setup ${setup.map((cmd) => `\`${cmd}\``).join(" then ")}`);
      }
      if (commands.length > 0) {
        const separator = coverageCommandsAreCumulative(family.family) ? " then " : " or ";
        parts.push(`run ${commands.map((cmd) => `\`${cmd}\``).join(separator)}`);
      }
      if (duration) parts.push(duration);
      const extra = parts.length > 0 ? ` — ${parts.join(" — ")}` : "";
      out.push(`- ${safeText(family.family)}${formats ? ` (expected: ${formats})` : ""}${extra}`);
    }
  }

  return out.join("\n");
}

function sanitizeBodyText(value: unknown): string {
  if (typeof value !== "string") return "";
  return Array.from(value, (char) => {
    const code = char.codePointAt(0) ?? 0;
    if (char === "\n" || char === "\r" || char === "\t") return char;
    if (code < 0x20 || code === 0x7f) return `\\u{${code.toString(16).padStart(4, "0")}}`;
    return char;
  }).join("");
}

function indentBlock(value: string): string {
  return value
    .split("\n")
    .map((line) => `    ${line}`)
    .join("\n");
}

function clipUtf8(value: string, maxBytes: number): { text: string; clipped: boolean } {
  const encoder = new TextEncoder();
  if (encoder.encode(value).byteLength <= maxBytes) return { text: value, clipped: false };
  const noteBytes = encoder.encode(ISSUE_CLIP_NOTE).byteLength;
  const contentLimit = Math.max(0, maxBytes - noteBytes);
  let bytes = 0;
  let text = "";
  for (const char of value) {
    const width = encoder.encode(char).byteLength;
    if (bytes + width > contentLimit) break;
    text += char;
    bytes += width;
  }
  return { text: text + ISSUE_CLIP_NOTE, clipped: true };
}

/**
 * Builds the external GitHub issue payload for a coverage snapshot.
 *
 * The local absolute repository path is deliberately excluded (and redacted
 * from optional MANVI prose), command output is never copied, producer-owned
 * text is indented as literal data, and the UTF-8 body stays below the backend's
 * 64 KiB hard limit. Filing still goes through the existing guarded issue owner.
 */
export function buildCoverageIssueDraft(
  report: CoverageReport,
  repoPath: string,
  manviAnalysis?: string | null,
): CoverageIssueDraft {
  const data = (report ?? {}) as Partial<CoverageReport>;
  const percentage = safePercent(data.overall?.percentage);
  const partial = data.truncated === true ? " (partial scan)" : "";
  const title = `test(coverage): address ${percentage} line coverage${partial}`;

  // The first rendered line contains the local path and is useful only for the
  // local model/clipboard. Drop it before this content crosses to GitHub.
  const snapshot = formatCoverageReport(report, "")
    .split("\n")
    .slice(1)
    .join("\n")
    .trimStart();
  let analysis = sanitizeBodyText(manviAnalysis).trim();
  if (repoPath) analysis = analysis.split(repoPath).join("<repository>");

  const sections = [
    "<!-- gitpulse:coverage-report:v1 -->",
    "## Coverage snapshot",
    "",
    "Generated by GitPulse from bounded coverage artifacts. No local absolute path or command output is included.",
    "",
    indentBlock(snapshot || "No coverage data was available."),
  ];
  if (analysis) {
    sections.push("", "## MANVI analysis", "", indentBlock(analysis));
  }
  sections.push(
    "",
    "## Resolution checklist",
    "",
    "- [ ] Confirm the report is current and complete.",
    "- [ ] Add tests for the lowest-covered behavior, including failure paths.",
    "- [ ] Regenerate coverage and attach the verified before/after totals.",
  );

  const clipped = clipUtf8(sections.join("\n"), MAX_ISSUE_BODY_BYTES);
  return { title, body: clipped.text, clipped: clipped.clipped };
}

export interface FailedCoverageScript {
  label: string;
  detail?: string | null;
}

export interface FailedCoverageDiagnosticsOptions {
  repoPath?: string | null;
  scanError?: string | null;
}

const GO_MISSING_MODULE =
  /directory prefix\s+\S+\s+does not contain main module/i;

const TEST_SUITE_RAN_FAILURE = [
  /Test Files\s+\d+\s+failed/i,
  /Failed Tests/i,
  /\bFAIL\s+\S+\.(t|j)sx?\b/i,
  /Tests:\s+\d+\s+failed/i,
  /not wrapped in act\s*\(/i,
  /React does not recognize the `[A-Za-z0-9$_]+` prop on a DOM element/i,
  /^FAILED\s+\S+/m,
  /^--- FAIL:/m,
];

/**
 * Classifies a failed coverage command so copied diagnostics name the real
 * problem: a missing Go module root, or a generator that ran and whose tests
 * failed — never "try a different ecosystem command" for those cases.
 */
export function coverageFailureHint(
  command: unknown,
  detail: unknown,
): string | null {
  const output = typeof detail === "string" ? detail : "";
  if (!output.trim()) return null;
  if (GO_MISSING_MODULE.test(output)) {
    return "Go was run from a directory without go.mod. Generate coverage from the module root with `go -C <module-dir> test ./... -coverprofile=coverage.out`.";
  }
  if (
    /No such file or directory \(os error 2\)/i.test(output) ||
    /Failed to spawn \S+/i.test(output)
  ) {
    return "That generator is not installed. GitPulse will not plan it unless the binary is on PATH.";
  }
  if (/outside the purpose-specific command allowlist/i.test(output)) {
    return "That command is not on the coverage allowlist. The scanner should not offer it.";
  }
  if (
    /npx canceled due to missing packages/i.test(output) ||
    /no YES option/i.test(output)
  ) {
    return "npx --no-install will not download a missing runner. Declare a coverage script or install the package locally.";
  }
  if (
    /Cannot find dependency '@vitest\/coverage-v8'/i.test(output) ||
    /MISSING DEPENDENCY/i.test(output)
  ) {
    return "Vitest ran without a coverage provider. Add @vitest/coverage-v8 or use the package.json coverage script.";
  }
  if (/wrapper ['`].*['`] is not a repository file/i.test(output)) {
    return "No Gradle wrapper in this repository. GitPulse will not invent ./gradlew.";
  }
  if (TEST_SUITE_RAN_FAILURE.some((pattern) => pattern.test(output))) {
    const cmd = typeof command === "string" ? command : "";
    const runner = /\bvitest\b|\bjest\b|npm run/.test(cmd)
      ? "The coverage generator ran; tests failed."
      : "Tests ran and failed.";
    return `${runner} This is a test failure, not the wrong ecosystem command.`;
  }
  return null;
}

function appendFailureBlock(out: string[], label: string, detail: string | null | undefined): void {
  out.push(`Command: ${label}`);
  out.push("Status: failed");
  const hint = coverageFailureHint(label, detail);
  if (hint) out.push(`Hint: ${hint}`);
  out.push("Output:");
  out.push(detail ? detail.trim() : "(no output recorded)");
}

/**
 * Formats failed coverage commands, generator scripts, and scan errors into a
 * clean plain-text diagnostic report suitable for copying to clipboard.
 */
export function formatFailedCoverageDiagnostics(
  failures: readonly FailedCoverageScript[],
  options?: FailedCoverageDiagnosticsOptions,
): string {
  const repo = options?.repoPath ? safeText(options.repoPath) : "";
  const scanErr = options?.scanError ? options.scanError.trim() : "";
  const validFailures = (failures ?? []).filter(
    (f): f is FailedCoverageScript => Boolean(f && typeof f === "object" && typeof f.label === "string"),
  );

  if (validFailures.length === 0 && !scanErr) {
    return "No coverage failures recorded.";
  }

  const out: string[] = [];
  out.push(repo ? `Coverage failure diagnostics — ${repo}` : "Coverage failure diagnostics");

  if (scanErr) {
    out.push(`Scan error: ${scanErr}`);
  }

  if (validFailures.length === 1) {
    const f = validFailures[0];
    if (scanErr) out.push("");
    appendFailureBlock(out, f.label, f.detail);
  } else if (validFailures.length > 1) {
    if (scanErr) out.push("");
    out.push(`Failed coverage commands (${validFailures.length}):`);
    validFailures.forEach((f, idx) => {
      out.push("");
      out.push(`[${idx + 1}] Command: ${f.label}`);
      out.push("Status: failed");
      const hint = coverageFailureHint(f.label, f.detail);
      if (hint) out.push(`Hint: ${hint}`);
      out.push("Output:");
      out.push(f.detail ? f.detail.trim() : "(no output recorded)");
    });
  }

  return out.join("\n");
}

