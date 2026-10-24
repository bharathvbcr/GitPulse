import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

describe("parseAttention unknown-event warning under many names", () => {
  let warn: ReturnType<typeof vi.spyOn>;
  beforeEach(() => {
    vi.resetModules();
    warn = vi.spyOn(console, "warn").mockImplementation(() => {});
  });
  afterEach(() => warn.mockRestore());

  it("still returns null for every unknown name, however many", async () => {
    const { parseAttention } = await import("./sessionActivity");
    for (let i = 0; i < 100; i++) {
      expect(parseAttention({ session: "t", channel: "hook", event: `future_${i}` }, 1)).toBeNull();
    }
  });

  it("says each distinct name once (spam bound)", async () => {
    const { parseAttention } = await import("./sessionActivity");
    for (let round = 0; round < 3; round++) {
      for (let i = 0; i < 100; i++) parseAttention({ session: "t", channel: "hook", event: `future_${i}` }, 1);
    }
    expect(warn.mock.calls.length).toBeLessThanOrEqual(17);
  });

  it("counts every row past the cap and says the total in a summary at most once a minute", async () => {
    const { parseAttention } = await import("./sessionActivity");
    for (let i = 0; i < 16; i++) parseAttention({ session: "t", channel: "hook", event: `old_${i}` }, 1);
    warn.mockClear();
    for (let i = 0; i < 5; i++) parseAttention({ session: "t", channel: "hook", event: `past_${i}` }, 10);
    expect(warn).toHaveBeenCalledTimes(1);
    expect(String(warn.mock.calls[0][0])).toContain("1 more");
    parseAttention({ session: "t", channel: "hook", event: "later" }, 60_011);
    expect(warn).toHaveBeenCalledTimes(2);
    expect(String(warn.mock.calls[1][0])).toContain("6 more");
    expect(String(warn.mock.calls[1][0])).toContain("later");
  });

  it("does not go silent for a brand-new name after the cap (a cap that hides the 17th is a silent drop)", async () => {
    const { parseAttention } = await import("./sessionActivity");
    for (let i = 0; i < 16; i++) parseAttention({ session: "t", channel: "hook", event: `old_${i}` }, 1);
    warn.mockClear();
    parseAttention({ session: "t", channel: "hook", event: "brand_new_event" }, 1);
    expect(warn).toHaveBeenCalled();
  });
});
