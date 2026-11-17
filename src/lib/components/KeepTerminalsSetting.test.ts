import { readFileSync } from "node:fs";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));
vi.mock("../platform", () => ({ isTauri: () => true }));

const { loadAgentDefaults, resetAgentDefaultsForTests } = await import("../stores/agentDefaultsStore");
const { keepsTerminalsRunning, withKeepTerminalsRunning } = await import("../terminal/agentDefaults");
const { default: KeepTerminalsSetting } = await import("./KeepTerminalsSetting.svelte");

const source = readFileSync(new URL("./KeepTerminalsSetting.svelte", import.meta.url), "utf8");

beforeEach(() => {
  invoke.mockReset();
  resetAgentDefaultsForTests();
});

describe("KeepTerminalsSetting", () => {
  it("compiles cleanly without Svelte warnings and wires through agentDefaults helpers", () => {
    const { warnings } = compile(source, { generate: "client", filename: "KeepTerminalsSetting.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
    expect(source).toContain("keepsTerminalsRunning(view.defaults)");
    expect(source).toContain("withKeepTerminalsRunning(view.defaults, next)");
  });

  it("renders checked by default when keep_terminals_running is absent alongside host and managed status", async () => {
    invoke.mockResolvedValue({
      defaults: { permission: {} },
      modes: ["edit"],
      launchers: ["claude"],
    });
    await loadAgentDefaults();

    const { body } = render(KeepTerminalsSetting, {
      props: {
        active: false,
        initialBackend: {
          backend: "host",
          trouble: [],
          managed: {
            requested: true,
            hosted: true,
            detail: "Managed agent runs are kept running when GitPulse quits, and the next launch picks them up.",
          },
        },
      },
    });

    expect(body).toContain("Keep managed agent runs running when GitPulse quits");
    expect(body).toContain('aria-checked="true"');
    expect(body).toContain('data-testid="keep-terminals-note"');
    expect(body).toContain("held by the terminal host (survive quit and crash)");
    expect(body).toContain('data-testid="keep-managed-status"');
    expect(body).toContain("Managed agent runs are kept running when GitPulse quits");
    expect(body).not.toContain('data-testid="keep-terminals-trouble"');
    expect(body).not.toContain('data-testid="keep-terminals-backend-error"');
  });

  it("renders unchecked when keep_terminals_running is explicitly false and saves opt-out/opt-in payloads cleanly", async () => {
    invoke.mockResolvedValue({
      defaults: { permission: { claude: "edit" }, keep_terminals_running: false },
      modes: ["edit"],
      launchers: ["claude"],
    });
    await loadAgentDefaults();

    const { body } = render(KeepTerminalsSetting, {
      props: {
        active: false,
        initialBackend: {
          backend: "host",
          trouble: [],
          managed: {
            requested: false,
            hosted: false,
            detail: "Managed agent runs end when GitPulse quits.",
          },
        },
      },
    });

    expect(body).toContain('aria-checked="false"');
    expect(body).toContain("Managed agent runs end when GitPulse quits.");

    const optOut = withKeepTerminalsRunning({ permission: { claude: "edit" } }, false);
    expect(optOut).toEqual({ permission: { claude: "edit" }, keep_terminals_running: false });
    expect(keepsTerminalsRunning(optOut)).toBe(false);

    const optIn = withKeepTerminalsRunning(optOut, true);
    expect(optIn).toEqual({ permission: { claude: "edit" } });
    expect(keepsTerminalsRunning(optIn)).toBe(true);
  });

  it("renders host trouble notices and backend read errors without summarizing them away", () => {
    const withTrouble = render(KeepTerminalsSetting, {
      props: {
        active: false,
        initialBackend: {
          backend: "host",
          trouble: ["Another version's terminal host holds 2 sessions (build 1.98.0)."],
        },
      },
    }).body;
    expect(withTrouble).toContain('data-testid="keep-terminals-trouble"');
    expect(withTrouble).toContain("Another version's terminal host holds 2 sessions (build 1.98.0).");

    const withError = render(KeepTerminalsSetting, {
      props: {
        active: false,
        initialBackendError: "host timed out",
      },
    }).body;
    expect(withError).toContain('data-testid="keep-terminals-backend-error"');
    expect(withError).toContain("Couldn't read which process holds the terminals. host timed out");
  });
});
