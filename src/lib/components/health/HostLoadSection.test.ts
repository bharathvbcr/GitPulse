import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { invoke } from "@tauri-apps/api/core";
import HostLoadSection from "./HostLoadSection.svelte";
import { hostLoadStatusFixture } from "../../../../harness/hostLoadFixture";
import { refreshHostLoad, resetHostLoadForTests } from "../../stores/hostLoadStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../../platform", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../platform")>();
  return { ...actual, isTauri: () => true };
});

afterEach(() => {
  vi.mocked(invoke).mockReset();
  resetHostLoadForTests();
});

describe("HostLoadSection", () => {
  it("renders Free disk with total when total_disk is present and omits 'of ...' when total_disk is unavailable", async () => {
    const withTotal = hostLoadStatusFixture();
    vi.mocked(invoke).mockResolvedValueOnce(withTotal);
    await refreshHostLoad();

    const fullBody = render(HostLoadSection).body;
    const withTotalDd = fullBody.match(/data-testid="host-load-free-disk">([^<]+)</)?.[1] ?? "";
    expect(withTotalDd).toContain(" of ");
    expect(fullBody).toContain("Measures immediate unallocated space available to GitPulse");

    const withoutTotal = hostLoadStatusFixture();
    withoutTotal.snapshot!.host.total_disk = { status: "unavailable", value: "older" };
    vi.mocked(invoke).mockResolvedValueOnce(withoutTotal);
    await refreshHostLoad();

    const olderBody = render(HostLoadSection).body;
    const withoutTotalDd = olderBody.match(/data-testid="host-load-free-disk">([^<]+)</)?.[1] ?? "";
    expect(withoutTotalDd).not.toBe("");
    expect(withoutTotalDd).not.toContain(" of ");
    expect(withoutTotalDd).not.toContain("not published");
  });
});
