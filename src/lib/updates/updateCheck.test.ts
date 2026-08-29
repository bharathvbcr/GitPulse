import { describe, expect, it, vi } from "vitest";
import {
  AUTO_CHECK_INTERVAL_MS,
  checkForAppUpdate,
  describeUpdateCheck,
  isDismissed,
  maybeNotifyUpdate,
  shouldAutoCheck,
  type UpdateCheck,
} from "./updateCheck";

function result(overrides: Partial<UpdateCheck> = {}): UpdateCheck {
  return {
    currentVersion: "0.0.3",
    latestVersion: "0.1.0",
    updateAvailable: true,
    releaseUrl: "https://github.com/bharathvbcr/GitPulse/releases/tag/v0.1.0",
    checked: true,
    error: null,
    ...overrides,
  };
}

describe("shouldAutoCheck", () => {
  it("never runs while the preference is off", () => {
    expect(
      shouldAutoCheck({ checkForUpdates: false, lastUpdateCheckAt: 0 }, 1_000_000),
    ).toBe(false);
    // Even with an ancient last-check, opting out wins.
    expect(
      shouldAutoCheck({ checkForUpdates: false, lastUpdateCheckAt: 1 }, 1e12),
    ).toBe(false);
  });

  it("runs on the first enabled launch", () => {
    expect(
      shouldAutoCheck({ checkForUpdates: true, lastUpdateCheckAt: 0 }, 1_000_000),
    ).toBe(true);
  });

  it("throttles to one check per interval", () => {
    const now = 1_000_000_000;
    const justChecked = { checkForUpdates: true, lastUpdateCheckAt: now - 1000 };
    expect(shouldAutoCheck(justChecked, now)).toBe(false);

    const stale = {
      checkForUpdates: true,
      lastUpdateCheckAt: now - AUTO_CHECK_INTERVAL_MS,
    };
    expect(shouldAutoCheck(stale, now)).toBe(true);
  });

  it("treats a future timestamp as just-checked, not as an elapsed interval", () => {
    // Clock moved backwards or a profile was restored: must not turn into a
    // check on every single launch.
    const now = 1_000_000_000;
    expect(
      shouldAutoCheck({ checkForUpdates: true, lastUpdateCheckAt: now + 5e9 }, now),
    ).toBe(false);
  });

  it("treats a corrupt timestamp as never checked", () => {
    const now = 1_000_000_000;
    for (const bad of [Number.NaN, Number.POSITIVE_INFINITY, -1]) {
      expect(
        shouldAutoCheck({ checkForUpdates: true, lastUpdateCheckAt: bad }, now),
      ).toBe(true);
    }
  });
});

describe("describeUpdateCheck", () => {
  it("distinguishes a failed check from an up-to-date one", () => {
    const failed = describeUpdateCheck(
      result({ checked: false, updateAvailable: false, error: "host unreachable" }),
    );
    expect(failed.kind).toBe("failed");
    expect(failed.message).toContain("host unreachable");

    const current = describeUpdateCheck(
      result({ updateAvailable: false, latestVersion: "0.0.3" }),
    );
    expect(current.kind).toBe("current");
    expect(current.message).not.toContain("Could not");
  });

  it("names both versions when an update exists", () => {
    const status = describeUpdateCheck(result());
    expect(status.kind).toBe("available");
    expect(status.message).toContain("0.1.0");
    expect(status.message).toContain("0.0.3");
  });

  it("still reads as failed with no error text", () => {
    const status = describeUpdateCheck(result({ checked: false, error: null }));
    expect(status.kind).toBe("failed");
  });
});

describe("isDismissed", () => {
  it("matches only the exact dismissed version", () => {
    expect(isDismissed(result({ latestVersion: "0.1.0" }), "0.1.0")).toBe(true);
    expect(isDismissed(result({ latestVersion: "0.2.0" }), "0.1.0")).toBe(false);
    expect(isDismissed(result(), "")).toBe(false);
  });

  it("never treats an unchecked result as dismissed", () => {
    expect(isDismissed(result({ checked: false }), "0.1.0")).toBe(false);
    expect(isDismissed(result({ updateAvailable: false }), "0.1.0")).toBe(false);
  });
});

describe("checkForAppUpdate", () => {
  it("passes the registered command name through the IPC seam", async () => {
    const invokeFn = vi.fn().mockResolvedValue(result());
    await checkForAppUpdate(invokeFn);
    expect(invokeFn).toHaveBeenCalledWith("cmd_check_app_update");
  });

  it("turns a rejected invoke into an unchecked result, not a throw", async () => {
    const invokeFn = vi.fn().mockRejectedValue(new Error("bridge torn down"));
    const out = await checkForAppUpdate(invokeFn);
    expect(out.checked).toBe(false);
    expect(out.updateAvailable).toBe(false);
    expect(out.error).toBe("bridge torn down");
  });

  it("stringifies a non-Error rejection", async () => {
    const out = await checkForAppUpdate(vi.fn().mockRejectedValue("plain string"));
    expect(out.checked).toBe(false);
    expect(out.error).toBe("plain string");
  });
});

describe("maybeNotifyUpdate", () => {
  function deps(overrides: Record<string, unknown> = {}) {
    return {
      prefs: {
        checkForUpdates: true,
        lastUpdateCheckAt: 0,
        dismissedUpdateVersion: "",
      },
      now: 1_000_000_000,
      check: vi.fn().mockResolvedValue(result()),
      markChecked: vi.fn(),
      notify: vi.fn(),
      onError: vi.fn(),
      ...overrides,
    };
  }

  it("makes no request at all while opted out", async () => {
    const d = deps({
      prefs: { checkForUpdates: false, lastUpdateCheckAt: 0, dismissedUpdateVersion: "" },
    });
    await expect(maybeNotifyUpdate(d)).resolves.toBe("skipped");
    expect(d.check).not.toHaveBeenCalled();
    expect(d.notify).not.toHaveBeenCalled();
  });

  it("notifies once for an available release", async () => {
    const d = deps();
    await expect(maybeNotifyUpdate(d)).resolves.toBe("notified");
    expect(d.notify).toHaveBeenCalledTimes(1);
    expect(d.markChecked).toHaveBeenCalledWith(d.now);
  });

  it("stays silent for a version already dismissed", async () => {
    const d = deps({
      prefs: {
        checkForUpdates: true,
        lastUpdateCheckAt: 0,
        dismissedUpdateVersion: "0.1.0",
      },
    });
    await expect(maybeNotifyUpdate(d)).resolves.toBe("current");
    expect(d.notify).not.toHaveBeenCalled();
    // The check still ran and still counts against the throttle.
    expect(d.markChecked).toHaveBeenCalledWith(d.now);
  });

  it("speaks up again for a newer version than the dismissed one", async () => {
    const d = deps({
      check: vi.fn().mockResolvedValue(result({ latestVersion: "0.2.0" })),
      prefs: {
        checkForUpdates: true,
        lastUpdateCheckAt: 0,
        dismissedUpdateVersion: "0.1.0",
      },
    });
    await expect(maybeNotifyUpdate(d)).resolves.toBe("notified");
    expect(d.notify).toHaveBeenCalledTimes(1);
  });

  it("routes a failed check to diagnostics and does not burn the throttle", async () => {
    const d = deps({
      check: vi.fn().mockResolvedValue(
        result({ checked: false, updateAvailable: false, error: "offline" }),
      ),
    });
    await expect(maybeNotifyUpdate(d)).resolves.toBe("failed");
    expect(d.onError).toHaveBeenCalledTimes(1);
    expect(d.notify).not.toHaveBeenCalled();
    // The next launch must retry rather than wait out a full day.
    expect(d.markChecked).not.toHaveBeenCalled();
  });

  it("never notifies when the running build is current", async () => {
    const d = deps({
      check: vi
        .fn()
        .mockResolvedValue(result({ updateAvailable: false, latestVersion: "0.0.3" })),
    });
    await expect(maybeNotifyUpdate(d)).resolves.toBe("current");
    expect(d.notify).not.toHaveBeenCalled();
  });
});
