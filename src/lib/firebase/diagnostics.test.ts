import { describe, expect, it } from "vitest";
import {
  collectAttempts,
  describeAccount,
  FAILURE_KIND_LABEL,
  formatFirebaseDiagnostics,
  type FirebaseDiagnosticsInput,
} from "./diagnostics";
import type {
  FirebaseAttempt,
  FirebaseBackendsReport,
  FirebaseFailureKind,
  FirebaseStatus,
} from "./types";

/** What the beta's probe recorded when it ran from `/` (captured 2026-11-14). */
const STARTUP_STDERR =
  "Error: Unable to obtain permissions for firebase-debug.log\n    at findAvailableLogFile (/opt/homebrew/lib/node_modules/firebase-tools/lib/logger.js:148:11)";

const attempt = (over: Partial<FirebaseAttempt> = {}): FirebaseAttempt => ({
  argv: ["firebase", "--version"],
  cwd: "/tmp",
  started_at_ms: Date.UTC(2026, 10, 14, 9, 0, 0),
  duration_ms: 812,
  exit_code: 1,
  stdout_tail: "",
  stderr_tail: STARTUP_STDERR,
  spawn_error: null,
  ...over,
});

const brokenCliStatus = (): FirebaseStatus => ({
  configured: true,
  projects: [{ alias: "default", project_id: "scholarlm-vbc" }],
  default_alias: "default",
  projects_truncated: false,
  has_apphosting_config: false,
  cli: {
    present: false,
    program: "firebase",
    version: null,
    reason: "Error: Unable to obtain permissions for firebase-debug.log",
    failure: {
      kind: "cli_startup",
      title: "The Firebase CLI could not start",
      next_step: "It could not create its debug log … not a Google Cloud permission.",
      permission: null,
      role: null,
      api: null,
    },
    attempt: attempt(),
  },
  rollout_listing: {
    available: false,
    checked: false,
    reason: "The Firebase CLI could not be run, so what it supports is unknown.",
    attempt: null,
  },
  firebaserc_error: null,
  firebasejson_error: null,
});

const input = (over: Partial<FirebaseDiagnosticsInput> = {}): FirebaseDiagnosticsInput => ({
  repoPath: "/Users/me/Code/scholarlm",
  appVersion: "1.99.0",
  generatedAt: Date.UTC(2026, 10, 14, 9, 5, 0),
  status: brokenCliStatus(),
  statusError: null,
  projectAlias: "",
  projectId: "",
  backendId: "",
  backends: null,
  backendsError: null,
  rollouts: null,
  rolloutsError: null,
  deployError: null,
  deploy: null,
  ...over,
});

const refusedBackends = (): FirebaseBackendsReport => ({
  available: false,
  checked: false,
  cli_present: true,
  project_id: "acme-prod",
  backends: [],
  truncated: false,
  unreachable: [],
  walk_incomplete: null,
  error: "HTTP Error: 403, Permission 'firebaseapphosting.backends.list' denied",
  failure: {
    kind: "missing_permission",
    title: "Missing permission firebaseapphosting.backends.list on acme-prod",
    next_step: "Ask a project owner to grant your account a role that includes firebaseapphosting.backends.list.",
    permission: "firebaseapphosting.backends.list",
    role: null,
    api: null,
  },
  attempts: [
    attempt({
      argv: ["firebase", "apphosting:backends:list", "--project", "acme-prod", "--non-interactive", "--json"],
      cwd: "/Users/me/Code/acme",
      started_at_ms: Date.UTC(2026, 10, 14, 9, 1, 0),
      stderr_tail: "",
      stdout_tail: '{"status":"error","error":"HTTP Error: 403"}',
    }),
    attempt({
      argv: ["firebase", "login:list"],
      cwd: "/Users/me/Code/acme",
      started_at_ms: Date.UTC(2026, 10, 14, 9, 1, 2),
      exit_code: 0,
      stderr_tail: "",
      stdout_tail: "Logged in as ada@example.com",
    }),
  ],
  account: { checked: true, email: "ada@example.com", reason: null },
});

describe("formatFirebaseDiagnostics", () => {
  it("carries everything a reader needs to act on the beta's startup failure", () => {
    const text = formatFirebaseDiagnostics(input());
    expect(text).toContain("Generated: 2026-11-14T09:05:00.000Z");
    expect(text).toContain("Waylode: 1.99.0");
    expect(text).toContain("Repository: /Users/me/Code/scholarlm");
    expect(text).toContain("Firebase CLI: firebase — not usable");
    // The classified state and its next step, not just the raw message.
    expect(text).toContain("Firebase CLI: CLI couldn't start — The Firebase CLI could not start");
    expect(text).toContain("not a Google Cloud permission");
    // The command that ran, where, when, and what it said.
    expect(text).toContain("$ firebase --version");
    expect(text).toContain("cwd: /tmp");
    expect(text).toContain("started 2026-11-14T09:00:00.000Z, took 812 ms, exit 1");
    expect(text).toContain("firebase-debug.log");
  });

  it("names the permission, the account and every command of a refused listing", () => {
    const text = formatFirebaseDiagnostics(
      input({
        status: { ...brokenCliStatus(), cli: { ...brokenCliStatus().cli, present: true, version: "15.34.2", failure: null, reason: null } },
        projectAlias: "default",
        projectId: "acme-prod",
        backends: refusedBackends(),
      }),
    );
    expect(text).toContain("Firebase CLI: firebase 15.34.2");
    expect(text).toContain("Project: default → acme-prod");
    expect(text).toContain("Signed-in account: ada@example.com");
    expect(text).toContain("Backend listing: Missing permission —");
    expect(text).toContain("Permission: firebaseapphosting.backends.list");
    expect(text).toContain("$ firebase apphosting:backends:list --project acme-prod");
    expect(text).toContain("$ firebase login:list");
    // Newest first: the account question ran after the listing.
    expect(text.indexOf("$ firebase login:list")).toBeLessThan(
      text.indexOf("$ firebase apphosting:backends:list"),
    );
  });

  it("never lets a credential the panel produced itself reach the clipboard", () => {
    const token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    const text = formatFirebaseDiagnostics(input({ deployError: `push failed with ${token}` }));
    expect(text).toContain("Deploy: push failed with");
    expect(text).not.toContain(token);
  });

  it("says a section is empty rather than leaving it out", () => {
    const text = formatFirebaseDiagnostics(
      input({ status: null, statusError: "IPC timed out", generatedAt: null }),
    );
    expect(text).not.toContain("Generated:");
    expect(text).toContain("Firebase CLI: not probed");
    expect(text).toContain("== Failures ==\nnone classified");
    expect(text).toContain("Status read: IPC timed out");
    expect(text).toContain("== Firebase commands (newest first) ==\nnone recorded");
  });

  it("reads payloads that predate the diagnostic fields without throwing", () => {
    const legacy = brokenCliStatus() as unknown as Record<string, unknown>;
    const cli = { ...(legacy.cli as Record<string, unknown>) };
    delete cli.failure;
    delete cli.attempt;
    const status = { ...legacy, cli } as unknown as FirebaseStatus;
    expect(() => formatFirebaseDiagnostics(input({ status }))).not.toThrow();
    expect(collectAttempts(input({ status }))).toEqual([]);
  });
});

describe("a refused deploy", () => {
  it("is reported as a classified failure with its command, never as a started rollout", () => {
    const text = formatFirebaseDiagnostics(
      input({
        projectAlias: "default",
        projectId: "acme-prod",
        backendId: "web",
        deploy: {
          project_id: "acme-prod",
          backend_id: "web",
          git_commit: "0123456789abcdef0123456789abcdef01234567",
          created: false,
          unconfirmed: null,
          error: "HTTP Error: 403, The caller does not have permission",
          failure: {
            kind: "missing_permission",
            title: "The signed-in account lacks a permission on acme-prod",
            next_step: "Ask a project owner to grant your account a role with access to Firebase App Hosting.",
            permission: null,
            role: null,
            api: null,
          },
          attempts: [
            attempt({
              argv: ["firebase", "apphosting:rollouts:create", "web", "--git-commit", "0123456789abcdef0123456789abcdef01234567"],
              started_at_ms: Date.UTC(2026, 10, 14, 9, 3, 0),
              stderr_tail: "HTTP Error: 403",
            }),
          ],
          account: { checked: true, email: "ada@example.com", reason: null },
        },
      }),
    );
    expect(text).toContain("Deploy: Missing permission —");
    expect(text).toContain("Deploy: HTTP Error: 403, The caller does not have permission");
    expect(text).toContain("$ firebase apphosting:rollouts:create web --git-commit");
    expect(text).toContain("Signed-in account: ada@example.com");
  });
});

describe("describeAccount", () => {
  it("keeps 'not asked', 'could not ask' and 'nobody' apart", () => {
    expect(describeAccount(null)).toContain("not asked");
    expect(describeAccount({ checked: false, email: null, reason: "spawn failed" })).toContain(
      "couldn't be asked (spawn failed)",
    );
    expect(describeAccount({ checked: true, email: null, reason: null })).toContain("nobody is signed in");
    expect(describeAccount({ checked: true, email: "ada@example.com", reason: null })).toBe("ada@example.com");
  });
});

describe("FAILURE_KIND_LABEL", () => {
  it("labels every failure state the backend can send", () => {
    const kinds: FirebaseFailureKind[] = [
      "cli_missing",
      "cli_startup",
      "local_files",
      "not_signed_in",
      "wrong_account",
      "missing_permission",
      "api_disabled",
      "network",
      "unknown",
    ];
    for (const kind of kinds) expect(FAILURE_KIND_LABEL[kind]).toBeTruthy();
    expect(new Set(kinds.map((kind) => FAILURE_KIND_LABEL[kind])).size).toBe(kinds.length);
  });
});
