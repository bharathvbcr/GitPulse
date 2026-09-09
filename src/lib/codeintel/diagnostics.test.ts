import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { copyText } from "../desktop/clipboard";
import { copyDevmapLogs, DEVMAP_LOG_READ_TIMEOUT_MS, formatDevmapLogs, type DevmapDiagnosticContext } from "./diagnostics";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../desktop/clipboard", () => ({ copyText: vi.fn() }));

const context: DevmapDiagnosticContext = {
  repository: "/repo", view: "navigator", building: true,
  cli: { available: false, reason: "devmap binary missing" },
  map: { available: false, reason: "map missing", path: "/repo/.devmap/repo_map.json" },
  graph: null,
  liveIndex: { phase: "failed", decision: null, reason: "store locked", updatedAt: 0, revision: 0, refreshing: false },
  errors: ["build refused"],
};

describe("DevMap diagnostic export", () => {
  beforeEach(() => { vi.resetAllMocks(); vi.mocked(copyText).mockResolvedValue(true); });
  afterEach(() => vi.useRealTimers());

  it("copies context and DevMap-only native logs through the shared clipboard", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(["now INFO [devmap] run=1 started", "now INFO [git] unrelated"])
      .mockResolvedValueOnce({ path: "/logs/gitpulse.log", lines: ["then INFO [devmap] last failure"], degraded: "older entries omitted" });
    expect(await copyDevmapLogs(context, () => true)).toBe("copied");
    const text = vi.mocked(copyText).mock.calls[0][0];
    expect(text).toContain("devmap binary missing");
    expect(text).toContain("store locked");
    expect(text).toContain("last failure");
    expect(text).toContain("older entries omitted");
    expect(text).toContain("1 DevMap entries in 2 sampled backend entries");
    expect(text).not.toContain("unrelated");
    expect(invoke).toHaveBeenCalledWith("cmd_diagnostic_log_tail", { maxLines: 500 });
  });

  it("copies explicit unavailability when both native reads fail", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("IPC unavailable"));
    expect(await copyDevmapLogs(context, () => true)).toBe("copied");
    expect(vi.mocked(copyText).mock.calls[0][0]).toContain("Current session log unavailable: IPC unavailable");
    expect(vi.mocked(copyText).mock.calls[0][0]).toContain("Durable log unavailable: IPC unavailable");
  });

  it("bounds stalled IPC reads and does not erase a successful source", async () => {
    vi.useFakeTimers();
    vi.mocked(invoke).mockImplementationOnce(() => new Promise(() => {}))
      .mockResolvedValueOnce({ path: "/logs", lines: ["now INFO [devmap] retained"], degraded: null });
    const copy = copyDevmapLogs(context, () => true);
    await vi.advanceTimersByTimeAsync(DEVMAP_LOG_READ_TIMEOUT_MS);
    expect(await copy).toBe("copied");
    const text = vi.mocked(copyText).mock.calls[0][0];
    expect(text).toContain("timed out");
    expect(text).toContain("retained");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not write the clipboard after a repository switch", async () => {
    vi.mocked(invoke).mockResolvedValue([]);
    expect(await copyDevmapLogs(context, () => false)).toBe("stale");
    expect(copyText).not.toHaveBeenCalled();
  });

  it("reports clipboard denial without claiming a copy", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("offline"));
    vi.mocked(copyText).mockResolvedValue(false);
    expect(await copyDevmapLogs(context, () => true)).toBe("failed");
  });

  it("redacts credentials and reports truncation on oversized diagnostic sections", () => {
    const secret = `ghp_${"A".repeat(36)}`;
    const line = `now INFO [devmap] ${secret} ${"x".repeat(200_000)} final failure`;
    const text = formatDevmapLogs(context, { status: "fulfilled", value: [line] },
      { status: "fulfilled", value: { path: "", lines: [], degraded: "logging unavailable" } });
    expect(text).not.toContain(secret);
    expect(text).toContain("section truncated");
    expect(text).toContain("final failure");
    expect(text).toContain("No DevMap entries in this sampled tail");
    expect(text.length).toBeLessThan(200_000);
  });

  it("keeps URL credential redaction with numeric and punctuation prefixes", () => {
    for (const prefix of ["", "9", "1.-", "prefix_", "x-"]) {
      const line = `now INFO [devmap] ${prefix}https://user:opaque-password@example.test`;
      const text = formatDevmapLogs(context, { status: "fulfilled", value: [line] },
        { status: "fulfilled", value: { path: "", lines: [], degraded: null } });
      expect(text).not.toContain("opaque-password");
      expect(text).toContain(`${prefix}https://user:<redacted>@example.test`);
    }
  });
});
