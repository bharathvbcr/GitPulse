/**
 * W3c adversary pass: render the REAL settings panel template with the hook
 * record's fields populated, which the lane's own test says is unreachable
 * under `environment: "node"`.
 *
 * It is reachable. A server render never runs the panel's `load` effect, so
 * `draft` stays null and the counts block is skipped. This test compiles a
 * copy of the real component whose only change is that `draft` starts loaded
 * (one line, asserted below so the copy cannot drift into a different
 * component), points the store at a backend view through the same mocks the
 * store's own test uses, and reads the rendered HTML.
 */
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));
vi.mock("../platform", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../platform")>()),
  isTauri: () => true,
}));

const here = dirname(fileURLToPath(import.meta.url));
const original = readFileSync(join(here, "SessionAlertSettings.svelte"), "utf8");
// Outside src/, in the ignored tmp/ at the repository root: see "the generated
// copy" below. The copy's relative imports are made absolute, so it loads the
// very modules (and mocks) the original would.
const scratch = join(here, "..", "..", "..", "tmp");
mkdirSync(scratch, { recursive: true });
const generatedDir = mkdtempSync(join(scratch, "vitest-generated-"));
const generatedPath = join(generatedDir, "AdvW3cSessionAlertSettingsLoaded.svelte");

const DRAFT_LINE = "let draft = $state<SessionAlertSettings | null>(null);";

function viewWith(status: Record<string, unknown>, bridgeSupported = true) {
  return {
    settings: {
      enabled: true,
      sound: false,
      shell_bell: false,
      configure_agents: true,
      hook_bridge: true,
      quiet_start: null,
      quiet_end: null,
    },
    status: {
      running: true,
      delivered: 4,
      suppressed_disabled: 0,
      suppressed_not_agent: 0,
      suppressed_attended: 0,
      suppressed_quiet: 0,
      coalesced: 0,
      resolved: 0,
      displaced: 0,
      rate_limited: 0,
      deferred: 0,
      dropped_queue: 0,
      dropped_scan: 0,
      failed: 0,
      bridge_rejected: 0,
      stale: 0,
      hook_replies_missed: 0,
      hook_reply_missed_last: null,
      hook_drop_record_error: null,
      bridge_listening: true,
      bridge_path: null,
      last_error: null,
      ...status,
    },
    bridge_supported: bridgeSupported,
  };
}

async function panel(view: ReturnType<typeof viewWith>): Promise<string> {
  vi.resetModules();
  const { invoke } = await import("../ipc/invoke");
  vi.mocked(invoke).mockResolvedValue(view);
  const store = await import("../stores/sessionAlertsStore");
  await store.loadSessionAlerts();
  const component = (await import(/* @vite-ignore */ generatedPath)).default;
  const { render } = await import("svelte/server");
  return render(component).body;
}

beforeAll(() => {
  expect(original).toContain(DRAFT_LINE);
  const loaded = original
    .replace(DRAFT_LINE, "let draft = $state<SessionAlertSettings | null>({ ...DEFAULT_SESSION_ALERT_SETTINGS });")
    .replace("  hookReplyMisses,\n", "  hookReplyMisses,\n    DEFAULT_SESSION_ALERT_SETTINGS,\n")
    .replace(/from "(\.\.?\/[^"]+)"/g, (_match, specifier: string) => `from "${join(here, specifier)}"`);
  expect(loaded).toContain("DEFAULT_SESSION_ALERT_SETTINGS,");
  expect(loaded).not.toMatch(/from "\.\.?\//);
  writeFileSync(generatedPath, loaded);
});

afterAll(() => {
  rmSync(generatedDir, { recursive: true, force: true });
});

describe("the generated copy", () => {
  it("is written outside src/, where the source-scanning contract tests cannot list it mid-run", () => {
    // Those tests list every .svelte file under src/ and then read each one;
    // a copy that exists between the two steps fails them with ENOENT.
    const src = join(here, "..", "..");
    expect(relative(src, generatedPath).startsWith("..")).toBe(true);
  });
});

describe("the real panel template with the hook record's fields set", () => {
  it("shows the ruled sentence, never summed with bridge_rejected, never 'lost'", async () => {
    const html = await panel(
      viewWith({
        hook_replies_missed: 3,
        hook_reply_missed_last: "session_ended at 2026-10-27T12:00:00.000Z",
        bridge_rejected: 40,
      }),
    );
    expect(html).toContain('data-testid="session-alerts-hook-misses"');
    expect(html).toContain("3 hook reports in the last 7 days got no reply from GitPulse in time");
    expect(html).toContain("(last: session_ended at 2026-10-27T12:00:00.000Z)");
    expect(html).toContain("A missed SessionEnd can leave that agent's banner standing; hooks do not retry.");
    // The refused-report line keeps its own number and nothing else.
    expect(html).toContain("40 hook reports were refused as malformed or not yours.");
    expect(html).not.toContain("43");
    expect(html).not.toMatch(/lost/);
  });

  it("renders the record error as its own role=alert, with no clean zero beside it", async () => {
    const html = await panel(
      viewWith({ hook_drop_record_error: "The record could not be read in full (x); the count may be too low." }),
    );
    expect(html).toMatch(
      /<p role="alert" data-testid="session-alerts-hook-record-error">The record could not be read in full/,
    );
    expect(html).not.toContain('data-testid="session-alerts-hook-misses"');
    expect(html).not.toMatch(/\b0 hook reports/);
  });

  it("says nothing about the record where the bridge is not supported", async () => {
    const html = await panel(
      viewWith(
        { hook_replies_missed: 2, hook_drop_record_error: "unreadable" },
        false,
      ),
    );
    expect(html).not.toContain("session-alerts-hook-misses");
    expect(html).not.toContain("session-alerts-hook-record-error");
  });

  it("still shows the count when the worker is not running (the record is the hook's, not the worker's)", async () => {
    const html = await panel(viewWith({ running: false, hook_replies_missed: 1 }));
    expect(html).toContain("The notification worker is not running");
    expect(html).toContain("1 hook report in the last 7 days got no reply from GitPulse in time");
  });

  it("shows no hook line for a clean zero", async () => {
    const html = await panel(viewWith({}));
    expect(html).not.toContain("session-alerts-hook-misses");
    expect(html).not.toContain("session-alerts-hook-record-error");
  });
});
