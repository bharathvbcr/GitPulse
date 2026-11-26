import "../../../src/app.css";
import { mount, tick, unmount } from "svelte";
import { emit } from "@tauri-apps/api/event";
import { mockIPCWithEvents } from "../../../harness/tauriMocks";
import { toastStore } from "../../../src/lib/stores/toastStore";
import ToastContainer from "../../../src/lib/components/ToastContainer.svelte";
import NativeNotificationBridgeView from "../../../src/lib/components/NativeNotificationBridgeView.svelte";
import SessionNotificationBridge from "../../../src/lib/components/SessionNotificationBridge.svelte";
import AttentionInbox from "../../../src/lib/components/AttentionInbox.svelte";

const LOGO_TRUNK_FRAGMENT = "M24.04 9.25A10.5";

const params = new URLSearchParams(location.search);
const scenario = params.get("scenario") ?? "all";
const theme = params.get("theme") === "light" ? "light" : "dark";
document.documentElement.classList.toggle("light", theme === "light");
document.documentElement.classList.toggle("dark", theme === "dark");

const crashes: string[] = [];
const unexpected: string[] = [];
window.addEventListener("error", (event) => crashes.push(event.message));
window.addEventListener("unhandledrejection", (event) => crashes.push(String(event.reason)));

const nowSec = Math.floor(Date.now() / 1000);
const deliveryItem = {
  id: "event-1",
  revision: 1,
  created_at: nowSec - 60,
  updated_at: nowSec - 30,
  state: "submitted",
  native_id: "gitpulse.0123456789abcdef0123456789abcdef.event-1",
  task_id: "task-1",
  activated_at: nowSec - 5,
  acknowledged_at: null,
};

const attentionItem = {
  id: "event-1",
  revision: 1,
  updated_at: nowSec - 30,
  source_sequence: 1,
  task_id: "task-1",
  task_revision: 1,
  target_type: "run",
  target_id: "run-1",
  target_revision: 1,
  target_status: "unavailable",
  kind: "run_unresolved",
  title: "Coding agent outcome needs attention",
  created_at: nowSec - 60,
  read_at: null,
  dismissed_at: null,
  snoozed_until: null,
};

const settingsItem = {
  id: "profile",
  revision: 1,
  updated_at: nowSec - 120,
  profile_id: "0123456789abcdef0123456789abcdef",
  enabled: true,
  sound: false,
  background: true,
  after_sequence: 0,
  quiet_start: null,
  quiet_end: null,
  muted_workspace_ids: [],
  muted_repository_ids: [],
  muted_task_ids: [],
};

let pendingActivated = scenario === "all" || scenario === "native-activation";
let acked = false;

mockIPCWithEvents((cmd, args) => {
  if (cmd.startsWith("plugin:")) return null;
  const payload = (args ?? {}) as Record<string, unknown>;
  if (cmd === "cmd_workbench_request") {
    const method = String(payload.method ?? "");
    if (method === "notifications.native.pending") {
      if (scenario === "native-error") {
        throw new Error("Fixture native notification store unavailable");
      }
      const items = pendingActivated && !acked ? [deliveryItem] : [];
      return JSON.stringify({
        ok: true,
        items,
        total: items.length,
        shown: items.length,
        has_more: false,
        next_cursor: null,
      });
    }
    if (method === "notifications.delivery.get") {
      return JSON.stringify({
        ok: true,
        item: acked ? { ...deliveryItem, revision: 2, acknowledged_at: nowSec } : deliveryItem,
      });
    }
    if (method === "notifications.ack") {
      acked = true;
      return JSON.stringify({
        ok: true,
        item: { ...deliveryItem, revision: 2, acknowledged_at: nowSec },
      });
    }
    if (method === "attention.get") {
      return JSON.stringify({ ok: true, item: attentionItem });
    }
    if (method === "attention.list") {
      return JSON.stringify({
        ok: true,
        items: [attentionItem],
        total: 1,
        shown: 1,
        has_more: false,
        next_cursor: null,
      });
    }
    if (method === "notifications.settings.get") {
      return JSON.stringify({ ok: true, item: settingsItem });
    }
    if (method === "notifications.native.status") {
      return JSON.stringify({
        ok: true,
        available: true,
        authorization: "authorized",
        error: null,
      });
    }
    unexpected.push(`${cmd}:${method}`);
    return JSON.stringify({ ok: false });
  }
  if (cmd === "cmd_terminal_sessions") return [];
  if (cmd === "cmd_session_attention_standing") return [];
  if (cmd === "cmd_resolve_repo") {
    return { path: "/repo", name: "GitPulse", is_bare: false, common_dir: "/repo/.git" };
  }
  unexpected.push(cmd);
  return null;
});

const settle = async (ms = 40) => {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, ms));
  await tick();
};

const until = async (pred: () => boolean, label: string) => {
  const end = Date.now() + 5000;
  while (!pred() && Date.now() < end) await settle(25);
  if (!pred()) throw new Error(`Timed out waiting for: ${label}`);
};

function inspectLogoIn(container: Element | null) {
  if (!container) {
    return {
      mounted: false,
      hasGitPulseLogo: false,
      visibleBox: false,
      rects: [] as { w: number; h: number; visible: boolean }[],
    };
  }
  const svgs = [...container.querySelectorAll("svg")];
  const logoSvgs = svgs.filter((svg) => {
    const html = svg.outerHTML;
    return html.includes(LOGO_TRUNK_FRAGMENT) || svg.getAttribute("aria-label") === "GitPulse";
  });
  const rects = logoSvgs.map((svg) => {
    const r = svg.getBoundingClientRect();
    const style = getComputedStyle(svg);
    const visible =
      r.width > 0 &&
      r.height > 0 &&
      style.display !== "none" &&
      style.visibility !== "hidden" &&
      Number.parseFloat(style.opacity || "1") > 0;
    return { w: r.width, h: r.height, visible };
  });
  return {
    mounted: true,
    hasGitPulseLogo: logoSvgs.some(
      (svg, idx) => svg.outerHTML.includes(LOGO_TRUNK_FRAGMENT) && Boolean(rects[idx]?.visible),
    ),
    visibleBox: rects.some((r) => r.visible),
    rects,
  };
}

const results: { name: string; pass: boolean; detail?: string }[] = [];
function check(name: string, assertion: () => boolean) {
  let pass = false;
  let detail: string | undefined;
  try {
    pass = Boolean(assertion());
  } catch (error) {
    detail = String(error);
  }
  results.push(detail ? { name, pass, detail } : { name, pass });
}
const surfaceSummary: Record<string, unknown> = {};
const mounted: Record<string, unknown>[] = [];

try {
  toastStore.clear();

  const toastHost = document.getElementById("toast-host")!;
  const nativeHost = document.getElementById("native-host")!;
  const sessionHost = document.getElementById("session-host")!;
  const inboxHost = document.getElementById("inbox-host")!;

  mounted.push(mount(ToastContainer, { target: toastHost }));
  mounted.push(mount(NativeNotificationBridgeView, { target: nativeHost }));
  mounted.push(mount(SessionNotificationBridge, { target: sessionHost }));
  mounted.push(
    mount(AttentionInbox, {
      target: inboxHost,
      props: {
        scope: { kind: "global" },
        active: true,
        onopen: async () => {},
      },
    }),
  );

  await settle(60);

  if (scenario === "all" || scenario === "toast") {
    toastStore.info("Sync completed cleanly");
    toastStore.error("Hook reply timed out", { label: "Retry", onClick: () => {} });
    await until(() => toastHost.querySelectorAll(".toast").length >= 1, "toast pile did not render");
    const pile = toastHost.querySelector('[data-testid="toast-pile"]');
    pile?.dispatchEvent(new MouseEvent("mouseenter", { bubbles: true }));
    await until(() => toastHost.querySelectorAll(".toast").length >= 2, "expanded toasts did not render");
  }

  if (scenario === "all" || scenario === "session-missed") {
    await emit("gitpulse-session-notification-open", "missing-session-e2e");
    await until(
      () => Boolean(sessionHost.querySelector(".session-activation")),
      "session activation notice did not render",
    );
  }

  if (scenario === "all" || scenario === "native-activation") {
    await until(
      () => Boolean(nativeHost.querySelector('[aria-label="Notification review"]')),
      "native activation dialog did not render",
    );
  } else if (scenario === "native-error") {
    await until(
      () => Boolean(nativeHost.querySelector(".activation-error")),
      "native activation error notice did not render",
    );
  }

  if (scenario === "all" || scenario === "inbox") {
    await until(
      () => Boolean(inboxHost.querySelector('[data-testid="notification-pile"] article')),
      "attention inbox item did not render",
    );
  }

  const toastEl = toastHost.querySelector('[role="region"][aria-label="Notifications"]');
  const nativeEl =
    nativeHost.querySelector('[role="dialog"][aria-label="Notification review"]') ??
    nativeHost.querySelector(".activation-error");
  const sessionEl = sessionHost.querySelector(".session-activation");
  const inboxEl = inboxHost.querySelector('section[aria-label="Inbox"]');

  surfaceSummary.toast = inspectLogoIn(toastEl);
  surfaceSummary.native = inspectLogoIn(nativeEl);
  surfaceSummary.session = inspectLogoIn(sessionEl);
  surfaceSummary.inbox = inspectLogoIn(inboxEl);

  check("notifications region mounts with accessible live regions", () => {
    const alertRegion = toastHost.querySelector('[role="alert"][aria-live="assertive"]');
    const statusRegion = toastHost.querySelector('[role="status"][aria-live="polite"]');
    return Boolean(toastEl && alertRegion && statusRegion);
  });

  if (scenario === "all" || scenario === "toast") {
    check("toast action and dismiss buttons remain exposed outside aria-hidden", () => {
      const buttons = [...toastHost.querySelectorAll(".toast button")];
      return buttons.length >= 3 && buttons.every((btn) => !btn.closest('[aria-hidden="true"]'));
    });

    const info = inspectLogoIn(toastEl);
    check("ToastContainer renders current GitPulse SVG logo with non-zero bounding box", () =>
      info.hasGitPulseLogo && info.visibleBox,
    );
  }

  if (scenario === "all" || scenario === "native-activation" || scenario === "native-error") {
    const info = inspectLogoIn(nativeEl);
    check("NativeNotificationBridgeView renders current GitPulse SVG logo with non-zero bounding box", () =>
      info.hasGitPulseLogo && info.visibleBox,
    );
  }

  if (scenario === "all" || scenario === "session-missed") {
    const info = inspectLogoIn(sessionEl);
    check("SessionNotificationBridge renders current GitPulse SVG logo with non-zero bounding box", () =>
      info.hasGitPulseLogo && info.visibleBox,
    );
  }

  if (scenario === "all" || scenario === "inbox") {
    const info = inspectLogoIn(inboxEl);
    check("AttentionInbox renders current GitPulse SVG logo with non-zero bounding box", () =>
      info.hasGitPulseLogo && info.visibleBox,
    );
  }

  check("no uncaught runtime errors", () => crashes.length === 0);
  check("no unexpected Tauri IPC calls", () => unexpected.length === 0);
} catch (error) {
  crashes.push(String(error));
  results.push({ name: "harness execution", pass: false, detail: String(error) });
} finally {
  toastStore.clear();
  for (const instance of mounted) {
    try {
      await unmount(instance);
    } catch {
      // Ignore unmount errors during cleanup.
    }
  }
}

const payload = {
  results,
  passed: results.filter((r) => r.pass).length,
  total: results.length,
  crashes,
  unexpected,
  errors: crashes,
  data: { scenario, theme, surfaces: surfaceSummary },
  diagnostics: JSON.stringify({ scenario, surfaces: surfaceSummary }),
};

document.getElementById("verdict")!.textContent =
  results.map((r) => `${r.pass ? "PASS" : "FAIL"} ${r.name}`).join("\n");
document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify(payload)));
const report = params.get("report");
if (report?.startsWith("/__gp_result/")) {
  await fetch(report, { method: "POST", body: document.documentElement.outerHTML });
}
