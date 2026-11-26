import "../../../src/app.css";
import { mount, tick, unmount } from "svelte";
import { mockIPCWithEvents } from "../../../harness/tauriMocks";
import { invoke } from "../../../src/lib/ipc/invoke";
import StatusPopover from "../../../src/lib/desktop/StatusPopover.svelte";
import type { MenuAgentRequest, MenuState } from "../../../src/lib/desktop/menuState";
import { makeMenuAgentRequest, makeMenuState, makeMenuStateWithRequests } from "../helpers/fixtures";

const LOGO_TRUNK_FRAGMENT = "M23.8 9.0A10.5";

const params = new URLSearchParams(location.search);
const scenario = params.get("scenario") ?? "waiting";
const theme = params.get("theme") === "light" ? "light" : "dark";
document.documentElement.classList.toggle("light", theme === "light");
document.documentElement.classList.toggle("dark", theme === "dark");

const stage = document.getElementById("stage")!;
const crashes: string[] = [];
const unexpected: string[] = [];
const dispatchedActions: { id: string; repoPath: string | null }[] = [];

window.addEventListener("error", (event) => crashes.push(event.message));
window.addEventListener("unhandledrejection", (event) => crashes.push(String(event.reason)));

mockIPCWithEvents((cmd, args) => {
  if (cmd.startsWith("plugin:")) return null;
  const payload = (args ?? {}) as Record<string, unknown>;
  if (cmd === "cmd_status_action") {
    const id = String(payload.id ?? "");
    const repoPath = (payload.repoPath as string | null | undefined) ?? null;
    dispatchedActions.push({ id, repoPath });
    if (scenario === "stale-repo" && repoPath === "/Projects/StaleRepo") {
      throw new Error("Repository changed. Review the current status and try again.");
    }
    return null;
  }
  unexpected.push(cmd);
  return null;
});

function buildScenarioSnapshot(): MenuState {
  const now = Date.now();
  const sampleReq = (
    idx: number,
    provider: MenuAgentRequest["provider"] = "claude",
    urgency: MenuAgentRequest["urgency"] = "blocking",
  ): MenuAgentRequest =>
    makeMenuAgentRequest({
      id: `term:pty-${provider}-${idx}`,
      source: "terminal",
      provider,
      agentLabel: `agent-${idx} (${provider})`,
      repoPath: idx === 2 ? "/Projects/DevCouncil" : "/Projects/GitPulse",
      repoLabel: idx === 2 ? "DevCouncil" : "GitPulse",
      urgency,
      reason: urgency === "error" ? "Stopped on an error" : "Needs your permission",
      detail: `Allow tool call #${idx}`,
      atMs: now - idx * 15_000,
    });

  if (scenario === "none") {
    return makeMenuState({
      agentsWaiting: 0,
      agentRequests: [],
      agentsRunning: 2,
      agentsInformational: 1,
    });
  }
  if (scenario === "overflow") {
    const requests = Array.from({ length: 8 }, (_, i) => sampleReq(i + 1, i % 2 === 0 ? "claude" : "agy"));
    return makeMenuStateWithRequests(requests, {
      agentsRunning: 2,
      agentsInformational: 1,
      agentRequestsHidden: 3,
    });
  }
  if (scenario === "conflict-jump") {
    return makeMenuStateWithRequests([sampleReq(1, "claude")], {
      conflicts: 2,
      agentsRunning: 1,
    });
  }
  if (scenario === "stale-repo") {
    return makeMenuStateWithRequests([sampleReq(1, "claude")], {
      agentsRunning: 1,
    });
  }
  return makeMenuStateWithRequests([sampleReq(1, "claude"), sampleReq(2, "agy")], {
    agentsRunning: 2,
    agentsInformational: 1,
  });
}

const settle = async (ms = 40) => {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, ms));
  await tick();
};

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

const snapshot = buildScenarioSnapshot();
let actionError: string | null = null;
const dataSummary: Record<string, unknown> = { scenario, theme };
let instance: Record<string, unknown> | undefined;

async function handleAction(id: string) {
  try {
    await invoke("cmd_status_action", {
      id,
      repoPath: scenario === "stale-repo" ? "/Projects/StaleRepo" : snapshot.activePath,
    });
  } catch (err) {
    actionError = err instanceof Error ? err.message : String(err);
  }
}

try {
  instance = mount(StatusPopover, {
    target: stage,
    props: {
      snapshot,
      material: "preview",
      onaction: (id: string) => void handleAction(id),
    },
  });

  await settle(60);

  const shell = stage.querySelector<HTMLElement>("[data-testid='status-popover']");
  const markSvg = stage.querySelector<SVGElement>(".mark svg");
  const markRect = markSvg?.getBoundingClientRect();
  const shellRect = shell?.getBoundingClientRect();

  dataSummary.shellHeight = shellRect?.height ?? null;
  dataSummary.markSvgHtml = markSvg?.outerHTML ?? null;

  check("StatusPopover renders header .mark with current GitPulse SVG logo", () =>
    Boolean(
      markSvg &&
        markSvg.outerHTML.includes(LOGO_TRUNK_FRAGMENT) &&
        markRect &&
        markRect.width > 0 &&
        markRect.height > 0,
    ),
  );

  check("StatusPopover height stays within 640px max popover ceiling", () =>
    Boolean(shellRect && shellRect.height > 0 && shellRect.height <= 640),
  );

  const focusRequestButtons = [
    ...stage.querySelectorAll<HTMLButtonElement>("button[data-testid='status-agent-request']"),
  ];
  dataSummary.renderedRequestButtons = focusRequestButtons.length;

  if (scenario === "none") {
    check("when no agents are waiting, no waiting-for-action request buttons render", () =>
      focusRequestButtons.length === 0,
    );
  } else {
    check("waiting-for-action agent request items are rendered in StatusPopover", () =>
      focusRequestButtons.length === snapshot.agentRequests.length && focusRequestButtons.length > 0,
    );

    const firstReq = snapshot.agentRequests[0];
    const firstBtn = focusRequestButtons[0];
    check(
      "agent request row renders canonical provider icon, agentLabel, reason, detail, and atMs",
      () => {
        if (!firstReq || !firstBtn) return false;
        const text = firstBtn.textContent ?? "";
        const hasProviderIcon = Boolean(
          firstBtn.querySelector(`[data-launcher-icon="${firstReq.provider}"]`),
        );
        const hasTime = Boolean(firstBtn.querySelector("time.request-age[datetime]"));
        return (
          hasProviderIcon &&
          hasTime &&
          text.includes(firstReq.agentLabel) &&
          text.includes(firstReq.reason) &&
          (firstReq.detail === null || text.includes(firstReq.detail))
        );
      },
    );

    if (firstBtn) {
      firstBtn.click();
      await settle(40);
    }

    check("clicking an agent request dispatches cmd_status_action with focus-agent:<id>", () =>
      Boolean(
        firstReq &&
          dispatchedActions.some((entry) => entry.id === `focus-agent:${firstReq.id}`),
      ),
    );

    if (scenario === "overflow") {
      check("overflow scenario indicates hidden agent requests without exceeding 640px height", () =>
        Boolean(
          shellRect &&
            shellRect.height <= 640 &&
            (stage.textContent?.includes("3 more") ||
              stage.textContent?.includes("+3") ||
              stage.textContent?.includes("hidden")),
        ),
      );
    }

    if (scenario === "stale-repo") {
      check("stale repo action rejection is caught cleanly", () =>
        Boolean(actionError && actionError.includes("Repository changed")),
      );
    }
  }

  // Exercise keyboard shortcuts: R -> refresh, 3 -> section:work:overview (when conflicts > 0), Escape -> dismiss
  window.dispatchEvent(new KeyboardEvent("keydown", { key: "r", bubbles: true, cancelable: true }));
  await settle(25);
  check("pressing R dispatches refresh action", () =>
    dispatchedActions.some((entry) => entry.id === "refresh"),
  );

  if (scenario === "conflict-jump") {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "3", bubbles: true, cancelable: true }));
    await settle(25);
    check("pressing 3 when conflicts exist dispatches section:work:overview", () =>
      dispatchedActions.some((entry) => entry.id === "section:work:overview"),
    );
  }

  window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
  await settle(25);
  check("pressing Escape dispatches dismiss action", () =>
    dispatchedActions.some((entry) => entry.id === "dismiss"),
  );

  dataSummary.dispatchedActions = dispatchedActions;
  dataSummary.actionError = actionError;

  check("no uncaught runtime errors", () => crashes.length === 0);
  check("no unexpected Tauri IPC calls", () => unexpected.length === 0);
} catch (error) {
  crashes.push(String(error));
  results.push({ name: "harness execution", pass: false, detail: String(error) });
} finally {
  if (instance) {
    try {
      await unmount(instance);
    } catch {
      // Ignore cleanup unmount errors.
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
  data: dataSummary,
  diagnostics: JSON.stringify(dataSummary),
};

document.getElementById("verdict")!.textContent =
  results.map((r) => `${r.pass ? "PASS" : "FAIL"} ${r.name}`).join("\n");
document.documentElement.setAttribute("data-gp-result", encodeURIComponent(JSON.stringify(payload)));
const report = params.get("report");
if (report?.startsWith("/__gp_result/")) {
  await fetch(report, { method: "POST", body: document.documentElement.outerHTML });
}
