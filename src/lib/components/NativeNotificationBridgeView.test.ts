import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { compile } from "svelte/compiler";
import { render } from "svelte/server";
import { describe, expect, it } from "vitest";
import NativeNotificationBridgeView from "./NativeNotificationBridgeView.svelte";
import type { Attention, NotificationDelivery } from "../workbench/client";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "NativeNotificationBridgeView.svelte"), "utf8");

const deliveryFixture = {
  id: "n1",
  task_id: "t1",
  native_id: "gitpulse.x.n1",
  revision: 1,
  state: "submitted",
  created_at: 1,
  updated_at: 1,
  activated_at: null,
  acknowledged_at: null,
} satisfies NotificationDelivery;

const noticeFixture = {
  id: "event-1",
  revision: 1,
  updated_at: 1,
  source_sequence: 1,
  task_id: "t1",
  task_revision: 1,
  target_type: "run",
  target_id: "r1",
  target_revision: 1,
  target_status: "task_deleted",
  kind: "run_exited",
  title: "Coding agent exited",
  created_at: 1,
  read_at: null,
  dismissed_at: null,
  snoozed_until: null,
} satisfies Attention;

describe("NativeNotificationBridgeView", () => {
  it("compiles cleanly and wraps <Logo in aria-hidden in both receipt and error cards", () => {
    const { warnings } = compile(source, { generate: "client", filename: "NativeNotificationBridgeView.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
    expect(source).toMatch(/import Logo from "\.\/Logo\.svelte";/);
    expect(source.match(/aria-hidden="true"[^>]*><Logo\b/g)).toHaveLength(2);
  });

  it("renders nothing when no activation or error is present", () => {
    const { body } = render(NativeNotificationBridgeView);
    expect(body).not.toContain("native-activation");
    expect(body).not.toContain("activation-error");
  });

  it("renders .receipt with the current GitPulse badge Logo and Close notification button", () => {
    const { body } = render(NativeNotificationBridgeView, {
      props: { initialCurrent: deliveryFixture, initialNotice: noticeFixture },
    });
    expect(body).toMatch(/class="receipt\b/);
    expect(body).toContain("Coding agent exited");
    expect(body).toContain("Close notification");
    expect(body).toContain("M24.04 9.25A10.5 10.5 0 1 0 26.5 16H22.6L20.2 21L16.8 9.9L14.4 16H11.8");
    expect(body).toMatch(/id="gp-[a-z0-9]+-badge"/);
    expect(body).toMatch(/<span class="receipt-logo[^"]*" aria-hidden="true">[\s\S]*?<svg[^>]*aria-label="GitPulse"/);
  });

  it("falls back to GitPulse activity heading when notice is missing or empty", () => {
    const missingNotice = render(NativeNotificationBridgeView, {
      props: { initialCurrent: deliveryFixture },
    }).body;
    expect(missingNotice).toMatch(/<strong[^>]*>GitPulse activity<\/strong>/);
    expect(missingNotice).toContain("Resolving the saved notification");

    const emptyTitle = render(NativeNotificationBridgeView, {
      props: { initialCurrent: deliveryFixture, initialNotice: { ...noticeFixture, title: "" } },
    }).body;
    expect(emptyTitle).toMatch(/<strong[^>]*>GitPulse activity<\/strong>/);
  });

  it("renders .activation-error with the current GitPulse badge Logo and Dismiss button", () => {
    const { body } = render(NativeNotificationBridgeView, {
      props: { initialError: "Notification activation failed" },
    });
    expect(body).toMatch(/class="activation-error gp-card shadow-float\b/);
    expect(body).toContain("Notification activation failed");
    expect(body).toContain("Dismiss");
    expect(body).toContain("M24.04 9.25A10.5 10.5 0 1 0 26.5 16H22.6L20.2 21L16.8 9.9L14.4 16H11.8");
    expect(body).toMatch(/id="gp-[a-z0-9]+-badge"/);
    expect(body).toMatch(/<span class="receipt-logo[^"]*" aria-hidden="true">[\s\S]*?<svg[^>]*aria-label="GitPulse"/);
  });
});
