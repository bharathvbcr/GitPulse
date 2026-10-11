// ft-3b404cb4: the exported invoke used to never time out.
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  // A backend call that never answers (hung gh / git).
  invoke: vi.fn(() => new Promise(() => {})),
}));

describe("ft-3b404cb4 the exported invoke has a deadline", () => {
  it("a never-resolving GitHub call settles (rejects) within 10 minutes of fake time", async () => {
    vi.useFakeTimers();
    const { invoke } = await import("./invoke");
    let settled: "pending" | "resolved" | "rejected" = "pending";
    invoke("cmd_github_context", { repoPath: "/r" }).then(
      () => (settled = "resolved"),
      () => (settled = "rejected"),
    );
    await vi.advanceTimersByTimeAsync(10 * 60_000);
    vi.useRealTimers();
    expect(settled).toBe("rejected");
  });
});
