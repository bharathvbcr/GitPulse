import { afterEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import type {
  CodeScanningAlertInfo,
  CodeScanningReport,
  DependabotAlertInfo,
  DependabotReport,
} from "./types";
import {
  GITHUB_CODE_SCANNING_COMMAND,
  GITHUB_DEPENDABOT_COMMAND,
  describeSeriousGithubAlerts,
  githubAlertsCache,
  isSeriousGithubSeverity,
  loadGithubAlerts,
  maybeNotifyGithubAlerts,
  seriousGithubFingerprint,
  type GithubAlertsSnapshot,
} from "./githubAlerts";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function dependabotAlert(
  overrides: Partial<DependabotAlertInfo> = {},
): DependabotAlertInfo {
  return {
    number: 1,
    package: "lodash",
    ecosystem: "npm",
    manifest_path: "package.json",
    scope: "runtime",
    severity: "high",
    title: "Prototype Pollution",
    advisory_id: "GHSA-xxxx",
    cve_id: "CVE-2020-8203",
    vulnerable_range: "< 4.17.19",
    first_patched: "4.17.19",
    url: "https://github.com/acme/repo/security/dependabot/1",
    created_at: "2026-01-01T00:00:00Z",
    ...overrides,
  };
}

function dependabotReport(
  overrides: Partial<DependabotReport> = {},
  alerts: DependabotAlertInfo[] = [dependabotAlert()],
): DependabotReport {
  return {
    available: true,
    cli_present: true,
    is_github_remote: true,
    slug: "acme/repo",
    alerts,
    truncated: false,
    error: null,
    ...overrides,
  };
}

function codeScanningAlert(
  overrides: Partial<CodeScanningAlertInfo> = {},
): CodeScanningAlertInfo {
  return {
    number: 1,
    rule_id: "js/sql-injection",
    rule_name: "sql-injection",
    severity: "high",
    state: "open",
    tool: "CodeQL",
    tool_version: "2.20.0",
    title: "SQL injection",
    path: "src/db.ts",
    start_line: 10,
    url: "https://github.com/acme/repo/security/code-scanning/1",
    dismissed_reason: "",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-02T00:00:00Z",
    ...overrides,
  };
}

function codeScanningReport(
  overrides: Partial<CodeScanningReport> = {},
  alerts: CodeScanningAlertInfo[] = [],
): CodeScanningReport {
  return {
    available: true,
    cli_present: true,
    is_github_remote: true,
    slug: "acme/repo",
    alerts,
    truncated: false,
    error: null,
    ...overrides,
  };
}

function snapshot(
  overrides: Partial<GithubAlertsSnapshot> = {},
): GithubAlertsSnapshot {
  return {
    dependabot: dependabotReport({}, []),
    dependabotRequestFailed: false,
    codeScanning: codeScanningReport(),
    codeScanningRequestFailed: false,
    checkedAt: 1_700_000_000_000,
    ...overrides,
  };
}

describe("isSeriousGithubSeverity", () => {
  it("treats critical, high, and CodeQL error as serious", () => {
    expect(isSeriousGithubSeverity("critical")).toBe(true);
    expect(isSeriousGithubSeverity("CRITICAL")).toBe(true);
    expect(isSeriousGithubSeverity("high")).toBe(true);
    expect(isSeriousGithubSeverity(" HIGH ")).toBe(true);
    expect(isSeriousGithubSeverity("error")).toBe(true);
  });

  it("does not promote medium, low, or unknown spellings", () => {
    expect(isSeriousGithubSeverity("medium")).toBe(false);
    expect(isSeriousGithubSeverity("moderate")).toBe(false);
    expect(isSeriousGithubSeverity("low")).toBe(false);
    expect(isSeriousGithubSeverity("warning")).toBe(false);
    expect(isSeriousGithubSeverity("note")).toBe(false);
    expect(isSeriousGithubSeverity("")).toBe(false);
    expect(isSeriousGithubSeverity("bogus")).toBe(false);
  });
});

describe("describeSeriousGithubAlerts", () => {
  it("is silent when nothing serious is known", () => {
    expect(describeSeriousGithubAlerts(snapshot())).toBeNull();
    expect(
      describeSeriousGithubAlerts(
        snapshot({
          dependabot: dependabotReport({}, [dependabotAlert({ severity: "low" })]),
        }),
      ),
    ).toBeNull();
    expect(
      describeSeriousGithubAlerts(
        snapshot({
          dependabot: dependabotReport({ available: false, error: "offline" }, []),
        }),
      ),
    ).toBeNull();
  });

  it("names critical and high counts, the slug, and both sources", () => {
    const text = describeSeriousGithubAlerts(
      snapshot({
        dependabot: dependabotReport({}, [
          dependabotAlert({ number: 1, severity: "critical" }),
        ]),
        codeScanning: codeScanningReport({}, [
          codeScanningAlert({ number: 7, severity: "error" }),
        ]),
      }),
    );
    expect(text).toBe(
      "1 critical and 1 high GitHub alerts on acme/repo (1 Dependabot, 1 code scanning).",
    );
  });

  it("says at least when either list was truncated", () => {
    const text = describeSeriousGithubAlerts(
      snapshot({
        dependabot: dependabotReport({ truncated: true }, [
          dependabotAlert({ severity: "HIGH" }),
        ]),
      }),
    );
    expect(text).toBe("At least 1 high GitHub alert on acme/repo.");
  });
});

describe("loadGithubAlerts", () => {
  it("folds a rejected invoke into the fail-closed envelope, not an empty success", async () => {
    const result = await loadGithubAlerts("/repo", {
      commands: {
        dependabot: () => Promise.reject(new Error("bridge down")),
        codeScanning: () => Promise.resolve(codeScanningReport()),
      },
      now: () => 42,
    });
    expect(result.dependabotRequestFailed).toBe(true);
    expect(result.dependabot.available).toBe(false);
    expect(result.dependabot.error).toBe("bridge down");
    expect(result.dependabot.alerts).toEqual([]);
    expect(result.codeScanning.available).toBe(true);
    expect(result.checkedAt).toBe(42);
  });

  it("shares one in-flight production fetch per path", async () => {
    // Injected commands skip the cache by design. The coalescing contract is
    // the production path, so this test only pins that two injected calls
    // stay independent — a cache hit must not be invented for fakes.
    const dependabot = vi.fn().mockResolvedValue(dependabotReport({}, []));
    const codeScanning = vi.fn().mockResolvedValue(codeScanningReport());
    const commands = { dependabot, codeScanning };
    await loadGithubAlerts("/a", { commands });
    await loadGithubAlerts("/a", { commands });
    expect(dependabot).toHaveBeenCalledTimes(2);
  });
});

describe("loadGithubAlerts production cache", () => {
  afterEach(() => {
    githubAlertsCache.clear();
    vi.mocked(invoke).mockReset();
  });

  function mockGithubInvoke(options?: {
    dependabot?: () => Promise<DependabotReport>;
    codeScanning?: () => Promise<CodeScanningReport>;
  }) {
    vi.mocked(invoke).mockImplementation((command) => {
      if (command === GITHUB_DEPENDABOT_COMMAND) {
        return options?.dependabot?.() ?? Promise.resolve(dependabotReport({}, []));
      }
      if (command === GITHUB_CODE_SCANNING_COMMAND) {
        return options?.codeScanning?.() ?? Promise.resolve(codeScanningReport());
      }
      return Promise.reject(new Error(`unexpected ${String(command)}`));
    });
  }

  it("coalesces concurrent production fetches for one path", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    mockGithubInvoke({
      dependabot: async () => {
        await gate;
        return dependabotReport({}, []);
      },
      codeScanning: async () => {
        await gate;
        return codeScanningReport();
      },
    });
    const first = loadGithubAlerts("/coalesce");
    const second = loadGithubAlerts("/coalesce");
    expect(second).toBe(first);
    release();
    await Promise.all([first, second]);
    const dependabotCalls = vi
      .mocked(invoke)
      .mock.calls.filter(([command]) => command === GITHUB_DEPENDABOT_COMMAND);
    expect(dependabotCalls).toHaveLength(1);
  });

  it("returns the cached snapshot on a later call", async () => {
    mockGithubInvoke();
    await loadGithubAlerts("/cached");
    await loadGithubAlerts("/cached");
    expect(
      vi
        .mocked(invoke)
        .mock.calls.filter(([command]) => command === GITHUB_DEPENDABOT_COMMAND),
    ).toHaveLength(1);
  });

  it("does not let a superseded fetch overwrite a later refresh", async () => {
    const resolvers: Array<(value: DependabotReport) => void> = [];
    mockGithubInvoke({
      dependabot: () =>
        new Promise<DependabotReport>((resolve) => {
          resolvers.push(resolve);
        }),
    });
    const first = loadGithubAlerts("/stale");
    await vi.waitFor(() => expect(resolvers).toHaveLength(1));
    const second = loadGithubAlerts("/stale", { force: true });
    await vi.waitFor(() => expect(resolvers).toHaveLength(2));
    expect(second).not.toBe(first);
    resolvers[1]?.(dependabotReport({ slug: "newer" }, []));
    await second;
    expect(githubAlertsCache.get("/stale")?.dependabot.slug).toBe("newer");
    resolvers[0]?.(dependabotReport({ slug: "older" }, []));
    await first;
    expect(githubAlertsCache.get("/stale")?.dependabot.slug).toBe("newer");
  });
});

describe("maybeNotifyGithubAlerts", () => {
  function deps(overrides: Record<string, unknown> = {}) {
    return {
      repoPath: "/repo",
      enabled: true,
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({}, [dependabotAlert()]),
        }),
      ),
      notify: vi.fn(),
      onError: vi.fn(),
      notified: new Map<string, string>(),
      ...overrides,
    };
  }

  it("makes no request while the preference is off", async () => {
    const d = deps({ enabled: false });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("skipped");
    expect(d.load).not.toHaveBeenCalled();
    expect(d.notify).not.toHaveBeenCalled();
  });

  it("makes no request without a repository", async () => {
    const d = deps({ repoPath: "" });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("skipped");
    expect(d.load).not.toHaveBeenCalled();
  });

  it("notifies once for critical or high findings", async () => {
    const d = deps();
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("notified");
    expect(d.notify).toHaveBeenCalledTimes(1);
    const message = d.notify.mock.calls[0][1] as string;
    expect(message).toContain("high");
    expect(message).toContain("acme/repo");
  });

  it("stays silent for the same serious set in one session", async () => {
    const d = deps();
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("notified");
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("clean");
    expect(d.notify).toHaveBeenCalledTimes(1);
  });

  it("speaks up again when a new serious alert appears", async () => {
    const first = snapshot({
      dependabot: dependabotReport({}, [dependabotAlert({ number: 1 })]),
    });
    const second = snapshot({
      dependabot: dependabotReport({}, [
        dependabotAlert({ number: 1 }),
        dependabotAlert({ number: 2, severity: "critical" }),
      ]),
    });
    const load = vi.fn().mockResolvedValueOnce(first).mockResolvedValueOnce(second);
    const d = deps({ load });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("notified");
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("notified");
    expect(d.notify).toHaveBeenCalledTimes(2);
  });

  it("does not toast medium-only findings", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({}, [
            dependabotAlert({ severity: "medium" }),
          ]),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("clean");
    expect(d.notify).not.toHaveBeenCalled();
    expect(d.onError).not.toHaveBeenCalled();
  });

  it("routes a failed check to diagnostics rather than a warning toast", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport(
            { available: false, error: "HTTP 403" },
            [],
          ),
          codeScanning: codeScanningReport({ available: false, error: "HTTP 403" }),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("failed");
    expect(d.onError).toHaveBeenCalledWith("HTTP 403");
    expect(d.notify).not.toHaveBeenCalled();
  });

  it("still warns about known serious alerts when the other check failed", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({}, [dependabotAlert({ severity: "critical" })]),
          codeScanning: codeScanningReport({ available: false, error: "HTTP 403" }),
          codeScanningRequestFailed: true,
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("notified");
    expect(d.notify).toHaveBeenCalledTimes(1);
    expect(d.onError).toHaveBeenCalledWith("HTTP 403");
  });

  it("does not treat a local-only repository as a failed check", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({
            available: false,
            is_github_remote: false,
            error: null,
            slug: "",
          }, []),
          codeScanning: codeScanningReport({
            available: false,
            is_github_remote: false,
            error: null,
            slug: "",
          }),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("unavailable");
    expect(d.notify).not.toHaveBeenCalled();
    expect(d.onError).not.toHaveBeenCalled();
  });
});

describe("seriousGithubFingerprint", () => {
  it("changes when truncation appears, so a cap cannot hide a worse set", () => {
    const open = snapshot({
      dependabot: dependabotReport({}, [dependabotAlert()]),
    });
    const capped = snapshot({
      dependabot: dependabotReport({ truncated: true }, [dependabotAlert()]),
    });
    expect(seriousGithubFingerprint(open)).not.toBe(
      seriousGithubFingerprint(capped),
    );
  });
});
