import { readFileSync } from "node:fs";
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
  isProductDisabledMessage,
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

function sourceFunctionBody(source: string, header: string): string {
  const start = source.indexOf(header);
  expect(start, `${header} must exist`).toBeGreaterThanOrEqual(0);
  const from = source.indexOf("{", start);
  expect(from, `${header} must open a body`).toBeGreaterThan(start);
  let depth = 0;
  for (let i = from; i < source.length; i++) {
    const ch = source[i];
    if (ch === "{") depth += 1;
    else if (ch === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(from, i + 1);
    }
  }
  throw new Error(`${header} body never closed`);
}

function quotedStrings(body: string): string[] {
  return [...body.matchAll(/"([^"]*)"/g)].map((match) => match[1]);
}

describe("isProductDisabledMessage", () => {
  it("is false for empty or whitespace", () => {
    expect(isProductDisabledMessage("")).toBe(false);
    expect(isProductDisabledMessage("   ")).toBe(false);
    expect(isProductDisabledMessage("\n\t")).toBe(false);
  });

  it("rejects similar but unknown prose", () => {
    expect(isProductDisabledMessage("Secret scanning is not enabled")).toBe(false);
    expect(isProductDisabledMessage("Code scanning is not configured")).toBe(false);
    expect(isProductDisabledMessage("Dependabot alerts disabled")).toBe(false);
    expect(isProductDisabledMessage("no analyses found")).toBe(false);
    expect(isProductDisabledMessage("code scanning is enabled")).toBe(false);
  });

  it("matches the phrases in Rust is_product_disabled_message", () => {
    const rust = readFileSync(
      new URL("../../../src-tauri/src/github/mod.rs", import.meta.url),
      "utf8",
    );
    const rustPhrases = quotedStrings(
      sourceFunctionBody(rust, "fn is_product_disabled_message"),
    );
    expect(
      rustPhrases,
      "is_product_disabled_message must list product-off phrases",
    ).not.toEqual([]);

    const js = readFileSync(new URL("./githubAlerts.ts", import.meta.url), "utf8");
    const jsPhrases = quotedStrings(
      sourceFunctionBody(js, "export function isProductDisabledMessage"),
    );
    expect(jsPhrases).toEqual(rustPhrases);

    for (const phrase of rustPhrases) {
      expect(isProductDisabledMessage(phrase), phrase).toBe(true);
      expect(isProductDisabledMessage(`  ${phrase.toUpperCase()} (HTTP 404)  `), phrase).toBe(
        true,
      );
    }
  });
});

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

  it("treats a null Dependabot payload as a failed check, not zero alerts", async () => {
    mockGithubInvoke({
      dependabot: async () => null as unknown as DependabotReport,
    });
    const result = await loadGithubAlerts("/null-dep");
    expect(result.dependabotRequestFailed).toBe(true);
    expect(result.dependabot.available).toBe(false);
    expect(result.dependabot.alerts).toEqual([]);
    expect(result.codeScanning.available).toBe(true);
  });

  it("treats a null code-scanning payload as a failed check, not zero alerts", async () => {
    mockGithubInvoke({
      codeScanning: async () => null as unknown as CodeScanningReport,
    });
    const result = await loadGithubAlerts("/null-cs");
    expect(result.codeScanningRequestFailed).toBe(true);
    expect(result.codeScanning.available).toBe(false);
    expect(result.codeScanning.alerts).toEqual([]);
    expect(result.dependabot.available).toBe(true);
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

  it("does not treat a disabled GitHub security product as a failed check", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({}, []),
          codeScanning: codeScanningReport({
            available: false,
            error:
              "Code scanning is not enabled for this repository. Please enable code scanning in the repository settings. (HTTP 404)",
            unavailable_reason: "product_disabled",
          }),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("clean");
    expect(d.notify).not.toHaveBeenCalled();
    expect(d.onError).not.toHaveBeenCalled();
  });

  it("does not warn for CodeQL never-run when Rust marks product_disabled", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({}, []),
          codeScanning: codeScanningReport({
            available: false,
            error: "no analysis found (HTTP 1)",
            unavailable_reason: "product_disabled",
          }),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("clean");
    expect(d.notify).not.toHaveBeenCalled();
    expect(d.onError).not.toHaveBeenCalled();
  });

  it("treats product-disabled prose without unavailable_reason as clean (0.1.0 flood gap)", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({}, []),
          codeScanning: codeScanningReport({
            available: false,
            error:
              "Code scanning is not enabled for this repository. Please enable code scanning in the repository settings. (HTTP 404)",
          }),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("clean");
    expect(d.notify).not.toHaveBeenCalled();
    expect(d.onError).not.toHaveBeenCalled();
  });

  it("treats no-analysis-found prose without unavailable_reason as clean (0.1.0 flood gap)", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({}, []),
          codeScanning: codeScanningReport({
            available: false,
            error: "no analysis found (HTTP 1)",
          }),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("clean");
    expect(d.notify).not.toHaveBeenCalled();
    expect(d.onError).not.toHaveBeenCalled();
  });

  it("still treats similar but unknown GitHub prose as a failed check", async () => {
    const unknown = [
      "Secret scanning is not enabled for this repository. (HTTP 404)",
      "Code scanning is not configured for this repository. (HTTP 404)",
      "Dependabot alerts disabled (HTTP 403)",
      "no analyses found (HTTP 1)",
    ];
    for (const error of unknown) {
      const d = deps({
        load: vi.fn().mockResolvedValue(
          snapshot({
            dependabot: dependabotReport({}, []),
            codeScanning: codeScanningReport({
              available: false,
              error,
            }),
          }),
        ),
      });
      await expect(maybeNotifyGithubAlerts(d), error).resolves.toBe("failed");
      expect(d.onError).toHaveBeenCalledWith(error);
      expect(d.notify).not.toHaveBeenCalled();
    }
  });

  it("still warns when a request failed even if the error text names a disabled product", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({}, []),
          codeScanning: codeScanningReport({
            available: false,
            error:
              "Code scanning is not enabled for this repository. Please enable code scanning in the repository settings. (HTTP 404)",
            unavailable_reason: "product_disabled",
          }),
          codeScanningRequestFailed: true,
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("failed");
    expect(d.onError).toHaveBeenCalledWith(
      "Code scanning is not enabled for this repository. Please enable code scanning in the repository settings. (HTTP 404)",
    );
  });

  it("treats Dependabot-disabled plus code-scanning-off as unavailable, not failed", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport(
            {
              available: false,
              error: "Dependabot alerts are disabled (HTTP 403)",
              unavailable_reason: "product_disabled",
            },
            [],
          ),
          codeScanning: codeScanningReport({
            available: false,
            error: "Advanced Security must be enabled for this repository to use code scanning. (HTTP 403)",
            unavailable_reason: "product_disabled",
          }),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("unavailable");
    expect(d.onError).not.toHaveBeenCalled();
  });

  it("does not treat a blank error as expected-unavailable or as a silent all-clear", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({ available: false, error: "   " }, []),
          codeScanning: codeScanningReport({ available: false, error: "   " }),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("failed");
    expect(d.onError).toHaveBeenCalledWith("   ");
    expect(d.notify).not.toHaveBeenCalled();
  });

  it("does not treat an empty error as expected-unavailable", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({ available: false, error: "" }, []),
          codeScanning: codeScanningReport({ available: false, error: "" }),
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("failed");
    expect(d.onError).toHaveBeenCalledWith("GitHub check returned no explanation.");
    expect(d.notify).not.toHaveBeenCalled();
  });

  it("still warns when a request failed even if the error text is empty", async () => {
    const d = deps({
      load: vi.fn().mockResolvedValue(
        snapshot({
          dependabot: dependabotReport({ available: false, error: "" }, []),
          codeScanning: codeScanningReport({
            available: false,
            error: "Code scanning is not enabled for this repository.",
          }),
          dependabotRequestFailed: true,
        }),
      ),
    });
    await expect(maybeNotifyGithubAlerts(d)).resolves.toBe("failed");
    expect(d.onError).toHaveBeenCalledWith("GitHub request failed.");
    expect(d.notify).not.toHaveBeenCalled();
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
