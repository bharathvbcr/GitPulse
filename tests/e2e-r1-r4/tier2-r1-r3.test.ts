import { existsSync, readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { inflateSync } from "node:zlib";
import { render } from "svelte/server";
import { get } from "svelte/store";
import { afterEach, describe, expect, it, vi } from "vitest";
import { clearMocks } from "@tauri-apps/api/mocks";
import { mockIPCWithEvents } from "../../harness/tauriMocks";
import AttentionInbox from "../../src/lib/components/AttentionInbox.svelte";
import Logo from "../../src/lib/components/Logo.svelte";
import SessionNotificationBridge from "../../src/lib/components/SessionNotificationBridge.svelte";
import ToastContainer from "../../src/lib/components/ToastContainer.svelte";
import WorkOverviewPane from "../../src/lib/components/WorkOverviewPane.svelte";
import WorkView from "../../src/lib/components/WorkView.svelte";
import StatusPopover from "../../src/lib/desktop/StatusPopover.svelte";
import { migrateViewTab, sanitizeViewSections } from "../../src/lib/repos/persist";
import { repoStore } from "../../src/lib/stores/repoStore";
import { toastStore } from "../../src/lib/stores/toastStore";
import {
  WORK_CONFLICTS_ID,
  conflictsFocusRequest,
  conflictsPanelOpen,
  openConflicts,
  setConflictsPanelOpen,
  takeConflictsFocus,
  takeConflictsReveal,
} from "../../src/lib/ui/workConflicts";
import { resolveSection } from "../../src/lib/views/viewRegistry";
import { runBrowserPage } from "./helpers/browser";
import {
  assertCargoPassed,
  assertTrayPulseIconUpdated,
  execCargoSuite,
  readPngInfo,
  readRepoFile,
  repoPath,
} from "./helpers/env";
import { makeRepoFixtures } from "./helpers/fixtures";

const RETIRED_LOGO_SHA256S = new Set([
  "eb41940073257e695b51f513061f56446da9b4fdf995a6aa06d41387ea7e6ea1",
  "1cf0341b1cc7b556701b5bfb17c3ed02b2eba2e87863928b4de8e1cc7225fd3b",
]);
const CANONICAL_PATH_FRAGMENT = "M23.8 9.0A10.5";

function decodePngRgbaPixels(relPath: string): {
  width: number;
  height: number;
  pixel: (x: number, y: number) => readonly [number, number, number, number];
} {
  const bytes = readFileSync(repoPath(relPath));
  const idat: Buffer[] = [];
  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  let offset = 8;

  while (offset + 12 <= bytes.length) {
    const len = bytes.readUInt32BE(offset);
    const kind = bytes.subarray(offset + 4, offset + 8).toString("ascii");
    const data = bytes.subarray(offset + 8, offset + 8 + len);
    offset += 12 + len;
    if (kind === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      bitDepth = data[8]!;
      colorType = data[9]!;
    } else if (kind === "IDAT") {
      idat.push(data);
    } else if (kind === "IEND") {
      break;
    }
  }

  expect(bitDepth).toBe(8);
  expect(colorType).toBe(6);

  const bpp = 4;
  const stride = width * bpp;
  const raw = inflateSync(Buffer.concat(idat));
  expect(raw.length).toBe(height * (1 + stride));

  const rgba = new Uint8Array(width * height * bpp);
  for (let y = 0; y < height; y += 1) {
    const filter = raw[y * (1 + stride)]!;
    const srcRow = y * (1 + stride) + 1;
    const dstRow = y * stride;
    for (let i = 0; i < stride; i += 1) {
      const x = raw[srcRow + i]!;
      const a = i >= bpp ? rgba[dstRow + i - bpp]! : 0;
      const b = y > 0 ? rgba[dstRow - stride + i]! : 0;
      const c = y > 0 && i >= bpp ? rgba[dstRow - stride + i - bpp]! : 0;
      let recon = x;
      if (filter === 1) recon = (x + a) & 0xff;
      else if (filter === 2) recon = (x + b) & 0xff;
      else if (filter === 3) recon = (x + ((a + b) >> 1)) & 0xff;
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        const pr = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
        recon = (x + pr) & 0xff;
      }
      rgba[dstRow + i] = recon;
    }
  }

  return {
    width,
    height,
    pixel: (x, y) => {
      const idx = (y * width + x) * 4;
      return [rgba[idx]!, rgba[idx + 1]!, rgba[idx + 2]!, rgba[idx + 3]!];
    },
  };
}

function findButtonsInsideAriaHidden(html: string): string[] {
  const VOID = new Set([
    "br",
    "hr",
    "img",
    "input",
    "meta",
    "link",
    "path",
    "circle",
    "line",
    "rect",
    "polyline",
  ]);
  const stack: { tag: string; hidden: boolean }[] = [];
  const trapped: string[] = [];
  for (const m of html.matchAll(
    /<!--[\s\S]*?-->|<(\/?)([a-zA-Z][\w-]*)([^>]*?)(\/?)>/g,
  )) {
    if (m[0].startsWith("<!--")) continue;
    const [, closing, tag, attrs, selfClose] = m;
    if (closing) {
      while (stack.length && stack.pop()!.tag !== tag);
      continue;
    }
    const hidden = /aria-hidden="true"/.test(attrs);
    if (tag === "button" && (hidden || stack.some((s) => s.hidden))) {
      trapped.push(attrs);
    }
    if (!selfClose && !VOID.has(tag)) {
      stack.push({ tag, hidden });
    }
  }
  return trapped;
}

async function withMockedOpenRepo(
  mode: "clean" | "dirty" | "conflicted" | "parked" | "probeFailed",
  fn: (repoDir: string) => Promise<void> | void,
): Promise<void> {
  const repoDir = "/workspace/GitPulse";
  const fx = makeRepoFixtures(mode === "probeFailed" ? "clean" : mode, repoDir);
  const hadWindow = Object.prototype.hasOwnProperty.call(globalThis, "window");
  const prevWindow = (globalThis as { window?: unknown }).window;

  if (!hadWindow || !prevWindow) {
    (globalThis as { window: unknown }).window = {
      addEventListener: () => {},
      removeEventListener: () => {},
      dispatchEvent: () => true,
    };
  }

  mockIPCWithEvents((cmd, args) => {
    if (cmd.startsWith("plugin:")) return null;
    const payload = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "cmd_resolve_repo":
        return {
          path: String(payload.repoPath ?? payload.path ?? repoDir),
          name: "GitPulse",
          is_bare: false,
          common_dir: `${repoDir}/.git`,
        };
      case "cmd_list_branches":
        return [
          {
            name: "main",
            is_current: true,
            is_remote: false,
            remote_name: null,
            tip_commit_id: "a".repeat(40),
            is_default: true,
            is_gone: false,
            last_commit_timestamp: Math.floor(Date.now() / 1000) - 600,
            last_author: "Alex",
            last_summary: "Work",
            upstream: "origin/main",
            ahead_count: 0,
            behind_count: 0,
          },
        ];
      case "cmd_get_status":
        return fx.statuses;
      case "cmd_repo_operation":
        if (mode === "probeFailed") {
          throw new Error("Simulated operation probe failure");
        }
        return fx.operation;
      case "cmd_list_tags":
        return { tags: [], truncated: false };
      case "cmd_stash_list":
        return { entries: [], truncated: false };
      case "cmd_list_worktrees":
        return fx.worktrees;
      case "cmd_get_commit_graph":
        return { nodes: [], edges: [], total_commits: 0, truncated: false };
      case "cmd_branch_stats":
        return { updates: [], capped: false, compute_failures: 0, compared_to: "main" };
      case "cmd_workspace_sync":
        return { repos: [] };
      case "cmd_list_repo_files":
        return fx.statuses.map((s) => s.path);
      case "cmd_task_view":
        return { available: false, store_path: "", leases: [], error: "" };
      case "cmd_worktree_task":
        return null;
      case "cmd_watch_repo":
      case "cmd_unwatch_repo":
      case "cmd_last_fetch_at":
      case "cmd_note_tab_activated":
      case "cmd_set_recent_menu":
        return null;
      default:
        return null;
    }
  });

  try {
    const opened = await repoStore.openRepo(repoDir);
    expect(opened).toBe(true);
    await fn(repoDir);
  } finally {
    const activeTabId = get(repoStore).activeTabId;
    if (activeTabId) {
      await repoStore.closeTab(activeTabId);
    }
    clearMocks();
    if (!hadWindow) {
      delete (globalThis as { window?: unknown }).window;
    } else {
      (globalThis as { window?: unknown }).window = prevWindow;
    }
  }
}

afterEach(() => {
  toastStore.clear();
  takeConflictsFocus();
  takeConflictsReveal();
  setConflictsPanelOpen(false);
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// F1 Boundaries — Logo Variants, IDs & Asset Integrity (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 2] [R1] F1 Boundaries — Logo Variants, IDs & Asset Integrity", () => {
  it("generates unique gp-* gradient IDs across multiple simultaneous Logo instances without collisions", () => {
    const instances = [
      render(Logo, { props: { variant: "mark", size: 16 } }).body,
      render(Logo, { props: { variant: "badge", size: 18 } }).body,
      render(Logo, { props: { variant: "badge", size: 24 } }).body,
    ];

    const seenIds = new Set<string>();
    for (const html of instances) {
      const ids = [
        ...html.matchAll(/id="(gp-[a-z0-9]+-(?:stroke|badge|glow))"/g),
      ].map((m) => m[1]!);
      const refs = [
        ...html.matchAll(/url\(#(gp-[a-z0-9]+-(?:stroke|badge|glow))\)/g),
      ].map((m) => m[1]!);

      expect(ids.length).toBeGreaterThanOrEqual(1);
      expect(refs.length).toBeGreaterThanOrEqual(1);
      for (const ref of refs) {
        expect(ids).toContain(ref);
      }
      for (const id of ids) {
        expect(seenIds.has(id), `colliding gradient id ${id}`).toBe(false);
        seenIds.add(id);
      }
    }
  });

  it("handles tiny (14px) and large (48px) size props and animated trace class cleanly", () => {
    const tiny = render(Logo, { props: { size: 14, animated: false } }).body;
    const large = render(Logo, { props: { size: 48, animated: true } }).body;

    expect(tiny).toContain('width="14"');
    expect(tiny).toContain('height="14"');
    expect(tiny).toContain('viewBox="0 0 32 32"');
    expect(tiny).not.toContain("wl-trace");

    expect(large).toContain('width="48"');
    expect(large).toContain('height="48"');
    expect(large).toContain('viewBox="0 0 32 32"');
    expect(large).toContain('pathLength="100"');
    expect(large).toContain("wl-trace");
  });

  it("enforces distinct transform and rounded plate bounds between badge and mark variants", () => {
    const badge = render(Logo, { props: { variant: "badge", size: 28 } }).body;
    const mark = render(Logo, { props: { variant: "mark", size: 28 } }).body;

    expect(badge).toContain('transform="translate(3.04 3.04) scale(0.81)"');
    expect(badge).toContain('<rect width="32" height="32" rx="10"');
    expect(badge).toContain('width="31.4" height="31.4" rx="9.7"');

    expect(mark).toContain('transform="translate(-2.4 -2.4) scale(1.15)"');
    expect(mark).not.toContain("<rect");
  });

  it("validates PNG colorType=6 RGBA, 8-bit depth, transparent corners, and Way-Arc pixels on plugin & app icons", () => {
    for (const rel of [
      "plugins/waylode/assets/logo.png",
      "src-tauri/icons/32x32.png",
      "src-tauri/icons/128x128.png",
      "src-tauri/icons/128x128@2x.png",
      "src-tauri/icons/icon.png",
    ]) {
      const info = readPngInfo(rel);
      expect(info.colorType, rel).toBe(6);
      expect(info.bitDepth, rel).toBe(8);
      expect(info.compression, rel).toBe(0);
      expect(info.filter, rel).toBe(0);
      expect(info.interlace, rel).toBe(0);
    }

    const decoded = decodePngRgbaPixels("plugins/waylode/assets/logo.png");
    expect(decoded.width).toBe(256);
    expect(decoded.height).toBe(256);
    expect(decoded.pixel(0, 0)[3]).toBe(0);
    const [gr, gg, gb, ga] = decoded.pixel(80, 176);
    expect(ga).toBeGreaterThan(200);
    expect(gb).toBeGreaterThan(gr);
    expect(gg).toBeGreaterThanOrEqual(180);
    const [sr, , sb, sa] = decoded.pixel(179, 83);
    expect(sa).toBeGreaterThan(200);
    expect(sb).toBeGreaterThan(sr);
    expect(sb).toBeGreaterThanOrEqual(230);
  });

  it("leaves zero stale logo assets across plugins/waylode and plugins/waylode-agy", () => {
    const assetDirs = ["plugins/waylode", "plugins/waylode-agy"];
    const foundAssets: string[] = [];

    for (const dirRel of assetDirs) {
      const entries = readdirSync(repoPath(dirRel), {
        recursive: true,
        withFileTypes: true,
      });
      for (const entry of entries) {
        if (!entry.isFile()) continue;
        if (!/\.(png|svg)$/i.test(entry.name)) continue;
        foundAssets.push(path.join(entry.parentPath, entry.name));
      }
    }

    expect(foundAssets.length).toBeGreaterThan(0);
    for (const absPath of foundAssets) {
      if (absPath.endsWith(".png")) {
        const info = readPngInfo(absPath);
        expect(RETIRED_LOGO_SHA256S.has(info.sha256), absPath).toBe(false);
      } else if (absPath.endsWith(".svg")) {
        const svg = readFileSync(absPath, "utf8");
        expect(svg, absPath).toContain(CANONICAL_PATH_FRAGMENT);
      }
    }
  });
});

// ---------------------------------------------------------------------------
// F2 Boundaries — macOS Notifications & Capability Boundaries (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 2] [R1] F2 Boundaries — macOS Notifications & Capability Boundaries", () => {
  it(
    "fails closed cleanly on unbundled macOS notification calls without panicking",
    async () => {
      const res = await execCargoSuite(
        ["--lib", "workbench::notifications::macos::tests"],
        { timeoutMs: 420_000 },
      );
      assertCargoPassed(res, "cargo test --lib workbench::notifications::macos::tests");
    },
    450_000,
  );

  it("preserves role=img and aria-label accessibility fallback for custom and empty Logo title props", () => {
    const custom = render(Logo, { props: { title: "Build Alert" } }).body;
    const empty = render(Logo, { props: { title: "" } }).body;

    expect(custom).toContain('role="img"');
    expect(custom).toContain('aria-label="Build Alert"');
    expect(empty).toContain('role="img"');
    expect(empty).toContain('aria-label=""');
  });

  it("preserves macOS notification category IDs, snooze action ID, and sound/thread flags", () => {
    const macosRs = readRepoFile("src-tauri/src/workbench/notifications/macos.rs");
    expect(macosRs).toContain('WORKBENCH_CATEGORY: &str = "gitpulse-workbench"');
    expect(macosRs).toContain('SESSION_CATEGORY: &str = "gitpulse-session"');
    expect(macosRs).toContain('SNOOZE_ACTION: &str = "snooze"');
    expect(macosRs).toContain("UNNotificationSound::defaultSound()");
    expect(macosRs).toContain("content.setThreadIdentifier");
  });

  it("validates tauri.conf.json resource bundle paths and icon files exist on disk", () => {
    const tauriConf = JSON.parse(readRepoFile("src-tauri/tauri.conf.json")) as {
      bundle?: { icon?: string[]; resources?: Record<string, string> };
    };
    const tauriDir = repoPath("src-tauri");

    for (const relIcon of tauriConf.bundle?.icon ?? []) {
      expect(existsSync(path.resolve(tauriDir, relIcon)), relIcon).toBe(true);
    }

    const pluginSrc = tauriConf.bundle?.resources?.["../plugins/gitpulse/"];
    expect(pluginSrc).toBe("plugin/");
    const resolvedPluginDir = path.resolve(tauriDir, "../plugins/gitpulse/");
    expect(existsSync(path.join(resolvedPluginDir, ".cursor-plugin/plugin.json"))).toBe(
      true,
    );
    expect(existsSync(path.join(resolvedPluginDir, "assets/logo.png"))).toBe(true);
  });

  it("isolates status-popover capability to 4 app commands on window status-popover and scopes default capability to main", () => {
    const statusCap = JSON.parse(
      readRepoFile("src-tauri/capabilities/status-popover.json"),
    ) as { windows?: string[]; permissions?: string[] };
    const defaultCap = JSON.parse(
      readRepoFile("src-tauri/capabilities/default.json"),
    ) as { windows?: string[]; permissions?: string[] };

    expect(statusCap.windows).toEqual(["status-popover"]);
    const appPerms = (statusCap.permissions ?? []).filter((p) => p.startsWith("allow-cmd-"));
    expect(appPerms).toHaveLength(4);
    expect(appPerms).toContain("allow-cmd-status-action");
    expect(appPerms).toContain("allow-cmd-get-status-state");
    expect(appPerms).toContain("allow-cmd-get-accessibility-display");
    expect(appPerms).toContain("allow-cmd-resize-status");
    expect(defaultCap.windows).toEqual(["main"]);
    expect(defaultCap.windows).not.toContain("*");
  });
});

// ---------------------------------------------------------------------------
// F3 Boundaries — In-App Notification Edge Cases (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 2] [R1] F3 Boundaries — In-App Notification Edge Cases", () => {
  it("renders ToastContainer collapsed stack (>1 toasts) with Logo, live-region announcements, and untrapped buttons", () => {
    toastStore.info("First notice", { label: "View", onClick: () => {} });
    toastStore.warning("Second notice");
    toastStore.success("Newest notice", { label: "Undo", onClick: () => {} });

    const { body } = render(ToastContainer);
    const pile = body.slice(body.indexOf('data-testid="toast-pile"'));
    expect(pile.match(/class="[^"]*\bpeek\b/g)).toHaveLength(2);
    expect(pile).toContain(CANONICAL_PATH_FRAGMENT);
    expect(pile).toContain("Newest notice");
    expect(body).toContain("First notice — View available");
    expect(body).toContain("Second notice");
    expect(findButtonsInsideAriaHidden(body)).toEqual([]);
  });

  it("renders Logo on all 4 ToastContainer severity kinds and separates assertive vs polite live regions", () => {
    for (const kind of ["error", "warning", "success", "info"] as const) {
      toastStore.clear();
      toastStore[kind](`Msg-${kind}`);
      const { body } = render(ToastContainer);
      expect(body, kind).toContain(CANONICAL_PATH_FRAGMENT);
      const alertBlock = body.slice(
        body.indexOf('role="alert"'),
        body.indexOf('role="status"'),
      );
      const statusBlock = body.slice(
        body.indexOf('role="status"'),
        body.indexOf('data-testid="toast-pile"'),
      );
      if (kind === "error") {
        expect(alertBlock).toContain(`Msg-${kind}`);
        expect(statusBlock).not.toContain(`Msg-${kind}`);
      } else {
        expect(statusBlock).toContain(`Msg-${kind}`);
        expect(alertBlock).not.toContain(`Msg-${kind}`);
      }
    }
  });

  it("renders NativeNotificationBridgeView error state without dialog/notice while displaying Logo and Dismiss", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/notifications-logo.html",
      query: { scenario: "native-error", theme: "dark" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    const nativeCheck = verdict.results.find((r) =>
      r.name.includes("NativeNotificationBridgeView renders current GitPulse SVG logo"),
    );
    expect(nativeCheck?.pass, JSON.stringify(verdict.results)).toBe(true);
    expect(verdict.data?.surfaces).toMatchObject({
      native: { mounted: true, hasGitPulseLogo: true, visibleBox: true },
    });
  });

  it("renders AttentionInbox Logo alongside filter options (active/unread/all) and dismiss/restore support", () => {
    const { body } = render(AttentionInbox, {
      props: { scope: { kind: "global" }, active: false, onopen: async () => {} },
    });
    expect(body).toContain(CANONICAL_PATH_FRAGMENT);
    expect(body).toContain('<option value="active"');
    expect(body).toContain('<option value="unread"');
    expect(body).toContain('<option value="all"');

    const inboxSrc = readRepoFile("src/lib/components/AttentionInbox.svelte");
    expect(inboxSrc).toContain('aria-label="Dismiss notification"');
    expect(inboxSrc).toContain(">Restore</button>");
  });

  it("renders SessionNotificationBridge empty when missed is null and renders Logo when missed notice is active", async () => {
    const emptyBody = render(SessionNotificationBridge).body;
    expect(emptyBody).not.toContain("session-activation");

    const activeVerdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/notifications-logo.html",
      query: { scenario: "session-missed", theme: "light" },
    });
    expect(activeVerdict.crashes).toEqual([]);
    expect(activeVerdict.unexpected).toEqual([]);
    const sessionCheck = activeVerdict.results.find((r) =>
      r.name.includes("SessionNotificationBridge renders current GitPulse SVG logo"),
    );
    expect(sessionCheck?.pass, JSON.stringify(activeVerdict.results)).toBe(true);
    expect(activeVerdict.data?.surfaces).toMatchObject({
      session: { mounted: true, hasGitPulseLogo: true, visibleBox: true },
    });
  });
});

// ---------------------------------------------------------------------------
// F4 Boundaries — Tray Glyph Pixels & StatusPopover Bounds (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 2] [R1] [R2] F4 Boundaries — Tray Glyph Pixels & StatusPopover Bounds", () => {
  it(
    "verifies tray.rs Plain vs Attention dot alpha and border bounds plus G-ring glyph implementation",
    async () => {
      const res = await execCargoSuite(["--lib", "desktop::tray::tests"], {
        timeoutMs: 280_000,
      });
      assertCargoPassed(res, "cargo test --lib desktop::tray::tests");

      const trayRs = readRepoFile("src-tauri/src/desktop/tray.rs");
      assertTrayPulseIconUpdated(trayRs);
    },
    300_000,
  );

  it("renders StatusPopover in light theme with visible Logo in .mark and height <= 640px in Chrome", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "waiting", theme: "light" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(Number(verdict.data?.shellHeight)).toBeLessThanOrEqual(640);
    const markCheck = verdict.results.find((r) =>
      r.name.includes("header .mark with current GitPulse SVG logo"),
    );
    expect(markCheck?.pass, JSON.stringify(verdict.results)).toBe(true);
  });

  it("SSR renders StatusPopover unavailable error state (snapshot = null, error set) with Logo rather than Activity", () => {
    const { body } = render(StatusPopover, {
      props: { snapshot: null, error: "Daemon disconnected", onaction: () => {} },
    });
    expect(body).toContain("Status unavailable");
    expect(body).toContain("Daemon disconnected");
    expect(body).toContain("Try again");
    expect(body).toContain(CANONICAL_PATH_FRAGMENT);
    expect(body).not.toContain("lucide-activity");
  });

  it("renders StatusPopover scenario=none with Logo in .mark, zero request buttons, and height <= 640px", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "none", theme: "dark" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.data?.renderedRequestButtons).toBe(0);
    expect(Number(verdict.data?.shellHeight)).toBeLessThanOrEqual(640);
    const markCheck = verdict.results.find((r) =>
      r.name.includes("header .mark with current GitPulse SVG logo"),
    );
    expect(markCheck?.pass, JSON.stringify(verdict.results)).toBe(true);
  });

  it("fits StatusPopover Logo cleanly inside the 34x34 .mark container without overflowing", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "waiting", theme: "dark" },
    });
    expect(verdict.crashes).toEqual([]);
    const markSvgHtml = String(verdict.data?.markSvgHtml ?? "");
    expect(markSvgHtml).toContain(CANONICAL_PATH_FRAGMENT);
    const widthMatch = /\bwidth="(\d+(?:\.\d+)?)"/.exec(markSvgHtml);
    const heightMatch = /\bheight="(\d+(?:\.\d+)?)"/.exec(markSvgHtml);
    expect(widthMatch).not.toBeNull();
    expect(heightMatch).not.toBeNull();
    expect(Number(widthMatch?.[1])).toBeGreaterThan(0);
    expect(Number(widthMatch?.[1])).toBeLessThanOrEqual(34);
    expect(Number(heightMatch?.[1])).toBeGreaterThan(0);
    expect(Number(heightMatch?.[1])).toBeLessThanOrEqual(34);
  });
});

// ---------------------------------------------------------------------------
// F5 Boundaries — Overview Conflicts & Routing Edge Cases (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 2] [R3] F5 Boundaries — Overview Conflicts & Routing Edge Cases", () => {
  it("handles no-repository-opened state gracefully in WorkView and WorkOverviewPane", () => {
    expect(get(repoStore).currentPath).toBeNull();
    const viewHtml = render(WorkView).body;
    expect(viewHtml).toContain("No repository open");
    const headerBtn = viewHtml.match(/<button[^>]*data-testid="overview-conflicts-btn"[^>]*>/)?.[0];
    expect(headerBtn).toBeDefined();
    expect(headerBtn).toContain("disabled");
    expect(viewHtml).not.toContain('data-testid="overview-repo-conflicts-btn"');

    const paneHtml = render(WorkOverviewPane, {
      props: { loadConflict: () => new Promise<never>(() => {}) },
    }).body;
    expect(paneHtml).toContain('aria-label="Overview"');
  });

  it("makes repeated rapid openConflicts() calls idempotent and single-consume", () => {
    const navigate = vi.spyOn(repoStore, "setActiveTab").mockImplementation(() => {});
    openConflicts();
    openConflicts();

    expect(navigate).toHaveBeenCalledTimes(2);
    expect(navigate).toHaveBeenLastCalledWith("work", "overview");
    expect(get(conflictsFocusRequest)).toBe(true);
    expect(takeConflictsReveal()).toBe(true);
    expect(takeConflictsReveal()).toBe(false);
    expect(takeConflictsFocus()).toBe(true);
    expect(takeConflictsFocus()).toBe(false);
  });

  it("redirects retired resolve/conflict routes cleanly to work/overview", () => {
    expect(resolveSection("work", "resolve")).toBe("overview");
    expect(resolveSection("work", "policy")).toBe("overview");
    expect(migrateViewTab("conflict")).toBe("work");
    expect(sanitizeViewSections({ work: "resolve" }, "conflict")).toEqual({
      work: "overview",
    });
  });

  it("resets manual reveal and panel-open state on repository switch and pane unmount", async () => {
    setConflictsPanelOpen(true);
    expect(get(conflictsPanelOpen)).toBe(true);
    setConflictsPanelOpen(false);
    expect(get(conflictsPanelOpen)).toBe(false);

    const verdict = await runBrowserPage<{
      panelOpenAfterReveal?: boolean;
      panelOpenAfterRepoSwitch?: boolean;
      panelOpenAfterUnmount?: boolean;
      layoutAfterClose?: string;
    }>({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "repo-switch" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed, JSON.stringify(verdict.results)).toBe(verdict.total);
    expect(verdict.data?.panelOpenAfterReveal).toBe(true);
    expect(verdict.data?.panelOpenAfterRepoSwitch).toBe(false);
    expect(verdict.data?.panelOpenAfterUnmount).toBe(false);
    expect(verdict.data?.layoutAfterClose).toBe("overview");
  }, 90_000);

  it("keeps Overview Conflicts/Resolve button visible and overflow-free in narrow 600px viewport in Chrome", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "clean", width: 600 },
      windowSize: { width: 600, height: 820 },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed).toBe(verdict.total);
    expect(verdict.data?.hasConflictsButton).toBe(true);
    expect(verdict.data?.overflowX).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// F6 Boundaries — Live Dominance, Hide Focus & Stress Cycles (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 2] [R3] F6 Boundaries — Live Dominance, Hide Focus & Stress Cycles", () => {
  it("cannot be stranded hidden when conflicted > 0, parked operation, or probeFailed is active", async () => {
    for (const mode of ["conflicted", "parked", "probeFailed"] as const) {
      await withMockedOpenRepo(mode, () => {
        const body = render(WorkOverviewPane, {
          props: { loadConflict: () => new Promise<never>(() => {}) },
        }).body;
        expect(body, mode).toContain('data-overview-layout="with-conflicts"');
        expect(body, mode).toContain(`id="${WORK_CONFLICTS_ID}"`);
        const barStart = body.indexOf('data-testid="work-conflicts-bar"');
        const scrollerStart = body.indexOf('data-overview-layout="with-conflicts"');
        expect(barStart, mode).toBeGreaterThan(-1);
        expect(scrollerStart, mode).toBeGreaterThan(barStart);
        const bar = body.slice(barStart, scrollerStart);
        expect(bar, mode).not.toContain(">Hide<");
      });
    }
  });

  it("returns cleanly to overview layout on Hide when clean without unmounting WorkView", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "clean" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed).toBe(verdict.total);
    expect(verdict.data?.hasHideButton).toBe(true);
    expect(verdict.data?.layoutAfterOpen).toBe("with-conflicts");
    expect(verdict.data?.layoutAfterClose).toBe("overview");
    expect(verdict.data?.workViewPreserved).toBe(true);
  });

  it("handles retained drafts boundary (0 drafts omits banner vs >0 drafts shows Review drafts & Hide)", async () => {
    await withMockedOpenRepo("clean", () => {
      const zeroDrafts = render(WorkOverviewPane, {
        props: { loadConflict: () => new Promise<never>(() => {}) },
      }).body;
      expect(zeroDrafts).not.toContain('data-testid="work-retained-drafts"');
    });

    const withDrafts = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "drafts" },
    });
    expect(withDrafts.crashes).toEqual([]);
    expect(withDrafts.unexpected).toEqual([]);
    expect(withDrafts.passed).toBe(withDrafts.total);
    expect(withDrafts.data?.hasHideButton).toBe(true);
  });

  it("preserves WorkView DOM identity and zero runtime errors across repeated open/hide/conflict/resolve cycles", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "transition" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed).toBe(verdict.total);
    expect(verdict.data?.workViewPreserved).toBe(true);
  });

  it("renders #work-conflicts in narrow 600px viewport with zero horizontal overflow and preserved WorkView identity", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "conflicted", width: 600 },
      windowSize: { width: 600, height: 820 },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed).toBe(verdict.total);
    expect(verdict.data?.overflowX).toBe(false);
    expect(verdict.data?.workViewPreserved).toBe(true);
  });
});
