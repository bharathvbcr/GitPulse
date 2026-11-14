/**
 * The Firebase panel's diagnostics: what the debug view shows and what
 * "Copy diagnostics" puts on the clipboard. One function produces both, so the
 * text a person pastes into a bug report is exactly the text they reviewed.
 *
 * Everything that came from the CLI was already redacted in Rust by the app's
 * canonical redactor before it crossed IPC. The assembled text is passed
 * through `redactDiagnosticText` once more, because it also carries strings the
 * panel produced itself (thrown IPC errors), which never went through Rust.
 *
 * A section that could not be filled says so. "Account: not asked" and
 * "Account: nobody is signed in" are different facts and are never merged.
 */
import { redactDiagnosticText } from "../diagnostics/diagnostics";
import type {
  FirebaseAccount,
  FirebaseAttempt,
  FirebaseBackendsReport,
  FirebaseFailure,
  FirebaseFailureKind,
  FirebaseRolloutsReport,
  FirebaseStatus,
  RolloutCreateOutcome,
} from "./types";

/** Short names for each failure state, as the panel and the report say them. */
export const FAILURE_KIND_LABEL: Record<FirebaseFailureKind, string> = {
  cli_missing: "CLI missing",
  cli_startup: "CLI couldn't start",
  local_files: "Local file access",
  not_signed_in: "Not signed in",
  wrong_account: "Wrong account",
  missing_permission: "Missing permission",
  api_disabled: "API disabled",
  network: "Network",
  unknown: "Unrecognised error",
};

export interface FirebaseDiagnosticsInput {
  repoPath: string | null;
  appVersion: string;
  /** Epoch ms the report was made; null leaves the line out (the live view). */
  generatedAt: number | null;
  status: FirebaseStatus | null;
  statusError: string | null;
  projectAlias: string;
  projectId: string;
  backendId: string;
  backends: FirebaseBackendsReport | null;
  backendsError: string | null;
  rollouts: FirebaseRolloutsReport | null;
  rolloutsError: string | null;
  deployError: string | null;
  /** The last deploy attempt's outcome, refused or started. */
  deploy: RolloutCreateOutcome | null;
}

/** A classified failure and which part of the panel it belongs to. */
export interface SourcedFailure {
  source: string;
  failure: FirebaseFailure;
}

/** Every classified failure on screen, CLI first. */
export function collectFailures(input: FirebaseDiagnosticsInput): SourcedFailure[] {
  const out: SourcedFailure[] = [];
  const cli = input.status?.cli?.failure;
  if (cli) out.push({ source: "Firebase CLI", failure: cli });
  const backends = input.backends?.failure;
  if (backends) out.push({ source: "Backend listing", failure: backends });
  const rollouts = input.rollouts?.failure;
  if (rollouts) out.push({ source: "Rollout listing", failure: rollouts });
  const deploy = input.deploy?.failure;
  if (deploy) out.push({ source: "Deploy", failure: deploy });
  return out;
}

/** Every recorded `firebase` invocation, newest first. */
export function collectAttempts(input: FirebaseDiagnosticsInput): FirebaseAttempt[] {
  const all: FirebaseAttempt[] = [];
  const push = (attempt: FirebaseAttempt | null | undefined) => {
    if (attempt) all.push(attempt);
  };
  push(input.status?.cli?.attempt);
  push(input.status?.rollout_listing?.attempt);
  for (const attempt of input.backends?.attempts ?? []) push(attempt);
  for (const attempt of input.rollouts?.attempts ?? []) push(attempt);
  for (const attempt of input.deploy?.attempts ?? []) push(attempt);
  return all.sort((a, b) => b.started_at_ms - a.started_at_ms);
}

/** The most recent account answer, or null when none was asked for. */
export function knownAccount(input: FirebaseDiagnosticsInput): FirebaseAccount | null {
  const candidates = [input.deploy?.account, input.rollouts?.account, input.backends?.account].filter(
    (account): account is FirebaseAccount => Boolean(account),
  );
  return candidates.find((account) => account.checked) ?? candidates[0] ?? null;
}

/** One line describing the account, honest about how much is known. */
export function describeAccount(account: FirebaseAccount | null): string {
  if (!account) return "not asked (only asked after a listing fails)";
  if (!account.checked) return `couldn't be asked${account.reason ? ` (${account.reason})` : ""}`;
  return account.email ?? "nobody is signed in to the Firebase CLI";
}

function isoTime(ms: number): string {
  const date = new Date(ms);
  return Number.isFinite(date.getTime()) ? date.toISOString() : String(ms);
}

function indent(text: string): string {
  return text
    .split("\n")
    .map((line) => `    ${line}`)
    .join("\n");
}

function describeCli(status: FirebaseStatus | null): string {
  const cli = status?.cli;
  if (!cli) return "not probed";
  if (cli.present) return `${cli.program} ${cli.version ?? "(version not reported)"}`;
  return `${cli.program} — not usable: ${cli.reason ?? "no reason reported"}`;
}

function describeProject(input: FirebaseDiagnosticsInput): string {
  if (!input.projectId) return "none selected";
  return input.projectAlias ? `${input.projectAlias} → ${input.projectId}` : input.projectId;
}

function formatAttempt(attempt: FirebaseAttempt): string {
  const exit =
    attempt.exit_code === null
      ? `no exit observed${attempt.spawn_error ? `: ${attempt.spawn_error}` : ""}`
      : `exit ${attempt.exit_code}`;
  const lines = [
    `$ ${attempt.argv.join(" ")}`,
    `  cwd: ${attempt.cwd}`,
    `  started ${isoTime(attempt.started_at_ms)}, took ${attempt.duration_ms} ms, ${exit}`,
  ];
  if (attempt.stderr_tail) lines.push("  stderr:", indent(attempt.stderr_tail));
  if (attempt.stdout_tail) lines.push("  stdout:", indent(attempt.stdout_tail));
  if (!attempt.stderr_tail && !attempt.stdout_tail && attempt.exit_code !== null) {
    lines.push("  (no output on either stream)");
  }
  return lines.join("\n");
}

function formatFailure({ source, failure }: SourcedFailure): string {
  const lines = [
    `${source}: ${FAILURE_KIND_LABEL[failure.kind] ?? failure.kind} — ${failure.title}`,
    `  Next step: ${failure.next_step}`,
  ];
  if (failure.permission) lines.push(`  Permission: ${failure.permission}`);
  if (failure.role) lines.push(`  Role: ${failure.role}`);
  if (failure.api) lines.push(`  API: ${failure.api}`);
  return lines.join("\n");
}

/**
 * The full diagnostics text, redacted.
 *
 * Sections are always present, each saying "none" when empty, so a reader can
 * tell an empty section from one that was cut.
 */
export function formatFirebaseDiagnostics(input: FirebaseDiagnosticsInput): string {
  const lines: string[] = ["GitPulse Firebase diagnostics"];
  if (input.generatedAt !== null) lines.push(`Generated: ${isoTime(input.generatedAt)}`);
  lines.push(
    `GitPulse: ${input.appVersion}`,
    `Repository: ${input.repoPath ?? "none open"}`,
    `Project: ${describeProject(input)}`,
    `Backend: ${input.backendId || "none selected"}`,
    `Firebase CLI: ${describeCli(input.status)}`,
    `Signed-in account: ${describeAccount(knownAccount(input))}`,
  );
  const listing = input.status?.rollout_listing;
  if (listing) {
    lines.push(
      `Rollout listing: ${
        listing.checked ? (listing.available ? "available" : "not in this CLI") : "not checked"
      }`,
    );
  }

  const failures = collectFailures(input);
  lines.push("", "== Failures ==");
  if (failures.length === 0) lines.push("none classified");
  else lines.push(...failures.map(formatFailure));

  const errors: [string, string | null | undefined][] = [
    ["Status read", input.statusError],
    ["Backend listing", input.backendsError ?? (input.backends?.checked === false ? input.backends.error : null)],
    ["Rollout listing", input.rolloutsError ?? (input.rollouts?.checked === false ? input.rollouts.error : null)],
    ["Deploy", input.deployError ?? (input.deploy?.created === false ? input.deploy.error : null)],
  ];
  const shown = errors.filter((entry): entry is [string, string] => Boolean(entry[1]));
  lines.push("", "== Errors as reported ==");
  if (shown.length === 0) lines.push("none");
  else lines.push(...shown.map(([label, message]) => `${label}: ${message}`));

  const attempts = collectAttempts(input);
  lines.push("", "== Firebase commands (newest first) ==");
  if (attempts.length === 0) lines.push("none recorded");
  else lines.push(attempts.map(formatAttempt).join("\n\n"));

  return redactDiagnosticText(lines.join("\n"));
}
