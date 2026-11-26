import { spawnSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { inflateSync } from "node:zlib";
import { render } from "svelte/server";
import { get } from "svelte/store";
import { afterEach, describe, expect, it, vi } from "vitest";
import { clearMocks } from "@tauri-apps/api/mocks";
import { mockIPCWithEvents } from "../../harness/tauriMocks";
import AttentionInbox from "../../src/lib/components/AttentionInbox.svelte";
import ConflictEditor from "../../src/lib/components/ConflictEditor.svelte";
import Logo from "../../src/lib/components/Logo.svelte";
import ToastContainer from "../../src/lib/components/ToastContainer.svelte";
import WorkOverviewPane from "../../src/lib/components/WorkOverviewPane.svelte";
import WorkView from "../../src/lib/components/WorkView.svelte";
import StatusPopover from "../../src/lib/desktop/StatusPopover.svelte";
import { buildCommands } from "../../src/lib/palette/catalog";
import { repoStore } from "../../src/lib/stores/repoStore";
import { toastStore } from "../../src/lib/stores/toastStore";
import {
  WORK_CONFLICTS_ID,
  conflictsFocusRequest,
  openConflicts,
  takeConflictsFocus,
  takeConflictsReveal,
} from "../../src/lib/ui/workConflicts";
import { runBrowserPage } from "./helpers/browser";
import {
  REPO_ROOT,
  assertCargoPassed,
  assertTrayPulseIconUpdated,
  execCargoSuite,
  readPngInfo,
  readRepoFile,
  repoPath,
} from "./helpers/env";
import { makeMenuState, makeRepoFixtures } from "./helpers/fixtures";

const RETIRED_LOGO_SHA256S = new Set([
  "eb41940073257e695b51f513061f56446da9b4fdf995a6aa06d41387ea7e6ea1",
  "1cf0341b1cc7b556701b5bfb17c3ed02b2eba2e87863928b4de8e1cc7225fd3b",
]);
const CANONICAL_PATH_FRAGMENT = "M23.8 9.0A10.5";
const CANONICAL_PATH_FULL =
  "M23.8 9.0A10.5 10.5 0 1 0 26.5 16.0H21.0L16.2 11.6L11.4 18.8";
const CANONICAL_STOPS = ["#7dd3fc", "#38bdf8", "#3b82f6"];

function extractGradientStops(svgText: string): string[] {
  const match = /<linearGradient[^>]*id="[^"]*stroke"[^>]*>([\s\S]*?)<\/linearGradient>/.exec(
    svgText,
  );
  if (!match) return [];
  return [...match[1].matchAll(/stop-color="(#[0-9a-fA-F]{6})"/g)].map((m) =>
    m[1].toLowerCase(),
  );
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
  mode: "clean" | "dirty" | "conflicted" | "parked",
  fn: (repoDir: string) => Promise<void> | void,
): Promise<void> {
  const repoDir = "/workspace/GitPulse";
  const fx = makeRepoFixtures(mode, repoDir);
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
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// F1 — Notification & Plugin Logo Assets (6 tests)
// ---------------------------------------------------------------------------
describe("[Tier 1] [R1] F1 — Notification & Plugin Logo Assets", () => {
  it("shares the canonical Way-Arc path, lodestar circle, and 3-stop gradient across static SVGs, index.html, and Logo.svelte", () => {
    const sources = [
      "src/lib/components/Logo.svelte",
      "public/waylode-icon.svg",
      "public/waylode-mark.svg",
      "index.html",
    ];
    for (const rel of sources) {
      const content = readRepoFile(rel);
      expect(content, `${rel} path`).toContain(CANONICAL_PATH_FULL);
      expect(content, `${rel} circle`).toMatch(
        /<circle[^>]*cx="23\.8"[^>]*cy="9(?:\.0)?"[^>]*r="2\.8"/,
      );
      expect(extractGradientStops(content), `${rel} gradient`).toEqual(CANONICAL_STOPS);
    }
  });

  it("ships plugins/waylode/assets/logo.png as current 256x256 RGBA Way-Arc badge matching 128x128@2x.png mark colors", () => {
    const pluginLogo = readPngInfo("plugins/waylode/assets/logo.png");
    const appIcon2x = readPngInfo("src-tauri/icons/128x128@2x.png");

    expect(pluginLogo.width).toBe(256);
    expect(pluginLogo.height).toBe(256);
    expect(pluginLogo.colorType).toBe(6);
    expect(pluginLogo.bitDepth).toBe(8);
    expect(RETIRED_LOGO_SHA256S.has(pluginLogo.sha256)).toBe(false);
    expect(appIcon2x.width).toBe(256);
    expect(appIcon2x.height).toBe(256);

    // Verify both 256x256 PNGs render the Cobalt light-sky->cobalt Way-Arc at the same coordinates
    const decode = (rel: string) => {
      const bytes = readFileSync(repoPath(rel));
      const idat: Buffer[] = [];
      let off = 8;
      while (off + 12 <= bytes.length) {
        const len = bytes.readUInt32BE(off);
        const kind = bytes.subarray(off + 4, off + 8).toString("ascii");
        const data = bytes.subarray(off + 8, off + 8 + len);
        off += 12 + len;
        if (kind === "IDAT") idat.push(data);
        else if (kind === "IEND") break;
      }
      const stride = 256 * 4;
      const raw = inflateSync(Buffer.concat(idat));
      const rgba = new Uint8Array(256 * stride);
      for (let y = 0; y < 256; y += 1) {
        const filter = raw[y * (1 + stride)]!;
        const s = y * (1 + stride) + 1;
        const d = y * stride;
        for (let i = 0; i < stride; i += 1) {
          const x = raw[s + i]!;
          const a = i >= 4 ? rgba[d + i - 4]! : 0;
          const b = y > 0 ? rgba[d - stride + i]! : 0;
          const c = y > 0 && i >= 4 ? rgba[d - stride + i - 4]! : 0;
          let recon = x;
          if (filter === 1) recon = (x + a) & 0xff;
          else if (filter === 2) recon = (x + b) & 0xff;
          else if (filter === 3) recon = (x + ((a + b) >> 1)) & 0xff;
          else if (filter === 4) {
            const p = a + b - c;
            const pr =
              Math.abs(p - a) <= Math.abs(p - b) && Math.abs(p - a) <= Math.abs(p - c)
                ? a
                : Math.abs(p - b) <= Math.abs(p - c)
                  ? b
                  : c;
            recon = (x + pr) & 0xff;
          }
          rgba[d + i] = recon;
        }
      }
      return (x: number, y: number) => {
        const idx = (y * 256 + x) * 4;
        return [rgba[idx]!, rgba[idx + 1]!, rgba[idx + 2]!, rgba[idx + 3]!] as const;
      };
    };

    const pluginPixel = decode("plugins/waylode/assets/logo.png");
    const iconPixel = decode("src-tauri/icons/128x128@2x.png");
    expect(pluginPixel(0, 0)[3]).toBe(0);
    // Light-sky portion (#7dd3fc)
    for (const [x, y] of [
      [80, 176],
      [73, 167],
    ] as const) {
      const [pr, pg, pb, pa] = pluginPixel(x, y);
      const [ir, ig, ib, ia] = iconPixel(x, y);
      expect(pa).toBeGreaterThan(200);
      expect(pb).toBeGreaterThan(pr);
      expect(pg).toBeGreaterThanOrEqual(180);
      expect(ia).toBeGreaterThan(200);
      expect(ib).toBeGreaterThan(ir);
      expect(ig).toBeGreaterThanOrEqual(180);
    }
    // Sky/cobalt portion (#38bdf8 -> #3b82f6)
    for (const [x, y] of [
      [128, 60],
      [179, 83],
    ] as const) {
      const [pr, , pb, pa] = pluginPixel(x, y);
      const [ir, , ib, ia] = iconPixel(x, y);
      expect(pa).toBeGreaterThan(200);
      expect(pb).toBeGreaterThan(pr);
      expect(pb).toBeGreaterThanOrEqual(230);
      expect(ia).toBeGreaterThan(200);
      expect(ib).toBeGreaterThan(ir);
      expect(ib).toBeGreaterThanOrEqual(230);
    }
  });

  it("references assets/logo.png in Cursor plugin manifest and bundles ../plugins/waylode/ in tauri.conf.json", () => {
    const cursorManifest = JSON.parse(
      readRepoFile("plugins/waylode/.cursor-plugin/plugin.json"),
    ) as { logo?: string };
    const tauriConf = JSON.parse(readRepoFile("src-tauri/tauri.conf.json")) as {
      bundle?: { resources?: Record<string, string> };
    };

    expect(cursorManifest.logo).toBe("assets/logo.png");
    expect(tauriConf.bundle?.resources?.["../plugins/waylode/"]).toBe("plugin/");
  });

  it("keeps src-tauri/icons/ PNGs and icon.icns valid and configured in tauri.conf.json", () => {
    const tauriConf = JSON.parse(readRepoFile("src-tauri/tauri.conf.json")) as {
      bundle?: { icon?: string[] };
    };
    const configured = tauriConf.bundle?.icon ?? [];
    expect(configured).toContain("icons/32x32.png");
    expect(configured).toContain("icons/128x128.png");
    expect(configured).toContain("icons/128x128@2x.png");
    expect(configured).toContain("icons/icon.icns");

    expect(readPngInfo("src-tauri/icons/32x32.png")).toMatchObject({
      width: 32,
      height: 32,
      colorType: 6,
    });
    expect(readPngInfo("src-tauri/icons/128x128.png")).toMatchObject({
      width: 128,
      height: 128,
      colorType: 6,
    });
    expect(readPngInfo("src-tauri/icons/128x128@2x.png")).toMatchObject({
      width: 256,
      height: 256,
      colorType: 6,
    });
    expect(readPngInfo("src-tauri/icons/icon.png")).toMatchObject({
      width: 512,
      height: 512,
      colorType: 6,
    });

    const icnsBytes = readFileSync(repoPath("src-tauri/icons/icon.icns"));
    expect(icnsBytes.subarray(0, 4).toString("ascii")).toBe("icns");
    expect(icnsBytes.length).toBeGreaterThan(100_000);
  });

  it("updates plugins/waylode-agy/assets/logo.svg to current Waylode Way-Arc geometry (not stale 3-node SVG)", () => {
    const agyManifest = JSON.parse(readRepoFile("plugins/waylode-agy/plugin.json")) as {
      logo?: string;
    };
    expect(agyManifest.logo).toBe("assets/logo.svg");

    const agyLogo = readRepoFile("plugins/waylode-agy/assets/logo.svg");
    expect(agyLogo).toContain(CANONICAL_PATH_FRAGMENT);
    expect(extractGradientStops(agyLogo)).toEqual(CANONICAL_STOPS);
    expect(agyLogo).not.toContain('fill="#0F172A"');
  });

  it("passes strict design-lint with zero raw #hex/rgb color literals in notification components", () => {
    const lint = spawnSync("node", ["scripts/design-lint.mjs", "--strict"], {
      cwd: REPO_ROOT,
      encoding: "utf8",
    });
    const combinedOutput = `${lint.stdout ?? ""}\n${lint.stderr ?? ""}`;

    const notificationComponents = [
      "src/lib/components/ToastContainer.svelte",
      "src/lib/components/NativeNotificationBridgeView.svelte",
      "src/lib/components/SessionNotificationBridge.svelte",
      "src/lib/components/AttentionInbox.svelte",
      "src/lib/desktop/StatusPopover.svelte",
    ];
    for (const rel of notificationComponents) {
      expect(combinedOutput, `${rel} mentioned in design-lint failures`).not.toContain(rel);
      const src = readRepoFile(rel);
      expect(src, `${rel} raw hex`).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
      expect(src, `${rel} raw rgb(`).not.toMatch(/\brgb\s*\(\s*\d/);
    }
  });
});

// ---------------------------------------------------------------------------
// F2 — macOS System Notification Logo & Bundle Delivery (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 1] [R1] F2 — macOS System Notification Logo & Bundle Delivery", () => {
  it(
    "passes Rust workbench::notifications tests",
    async () => {
      const res = await execCargoSuite(["--lib", "workbench::notifications"], {
        timeoutMs: 280_000,
      });
      assertCargoPassed(res, "cargo test --lib workbench::notifications");
    },
    300_000,
  );

  it("configures bundled macOS notification delivery with icon.icns and unbundled safety", () => {
    const macosRs = readRepoFile("src-tauri/src/workbench/notifications/macos.rs");
    const notifRs = readRepoFile("src-tauri/src/workbench/notifications.rs");
    const tauriConf = JSON.parse(readRepoFile("src-tauri/tauri.conf.json")) as {
      bundle?: { icon?: string[] };
    };

    expect(tauriConf.bundle?.icon).toContain("icons/icon.icns");
    expect(existsSync(repoPath("src-tauri/icons/icon.icns"))).toBe(true);
    expect(macosRs).toContain("NSBundle::mainBundle().bundleIdentifier().is_none()");
    expect(macosRs).toContain("UNMutableNotificationContent::new()");
    expect(macosRs).toContain("setCategoryIdentifier");
    expect(macosRs).toContain("setThreadIdentifier");
    expect(notifRs).toContain("platform::post");
  });

  it(
    "keeps default.json and status-popover.json capability contracts valid",
    async () => {
      const res = await execCargoSuite(["--test", "acl_contract"], {
        timeoutMs: 280_000,
      });
      assertCargoPassed(res, "cargo test --test acl_contract");
    },
    300_000,
  );

  it("renders distinct mark and badge Logo variants with accessible role=img and aria-label", () => {
    const markHtml = render(Logo, { props: { variant: "mark", size: 24 } }).body;
    const badgeHtml = render(Logo, { props: { variant: "badge", size: 24 } }).body;

    expect(markHtml).toContain('role="img"');
    expect(markHtml).toContain('aria-label="Waylode"');
    expect(markHtml).toContain(CANONICAL_PATH_FRAGMENT);
    expect(markHtml).toContain('transform="translate(-2.4 -2.4) scale(1.15)"');
    expect(markHtml).not.toContain('rx="10"');

    expect(badgeHtml).toContain('role="img"');
    expect(badgeHtml).toContain('aria-label="Waylode"');
    expect(badgeHtml).toContain(CANONICAL_PATH_FRAGMENT);
    expect(badgeHtml).toContain('transform="translate(3.04 3.04) scale(0.81)"');
    expect(badgeHtml).toContain('rx="10"');
  });

  it("contains no stale pulse-only or retired image or SVG artifacts under plugins/waylode/assets/", () => {
    const assetsDir = repoPath("plugins/waylode/assets");
    const files = readdirSync(assetsDir, { recursive: true, withFileTypes: true })
      .filter((d) => d.isFile())
      .map((d) => path.join(d.parentPath, d.name));

    expect(files.length).toBeGreaterThan(0);
    for (const file of files) {
      if (file.endsWith(".png")) {
        const info = readPngInfo(file);
        expect(RETIRED_LOGO_SHA256S.has(info.sha256), file).toBe(false);
      } else if (file.endsWith(".svg")) {
        const svg = readFileSync(file, "utf8");
        expect(svg, file).toContain(CANONICAL_PATH_FRAGMENT);
      }
    }
  });
});

// ---------------------------------------------------------------------------
// F3 — In-App Notification Surfaces Logo (6 tests)
// ---------------------------------------------------------------------------
describe("[Tier 1] [R1] F3 — In-App Notification Surfaces Logo", () => {
  it("SSR renders ToastContainer with current Waylode Logo, exposed action/dismiss buttons, and separate live regions", () => {
    toastStore.info("Workspace indexed cleanly", {
      label: "Open",
      onClick: () => {},
    });
    const { body } = render(ToastContainer);

    expect(body).toContain('role="region"');
    expect(body).toContain('aria-label="Notifications"');
    expect(body).toContain('aria-live="assertive"');
    expect(body).toContain('aria-live="polite"');
    expect(body).toContain(CANONICAL_PATH_FRAGMENT);
    expect(body).toContain('aria-label="Waylode"');
    expect(body).toContain('aria-label="Open: Workspace indexed cleanly"');
    expect(body).toContain('aria-label="Dismiss: Workspace indexed cleanly"');
    expect(findButtonsInsideAriaHidden(body)).toEqual([]);
  });

  it("renders current GitPulse Logo in NativeNotificationBridgeView activation dialog and error banner in Chrome", async () => {
    const activation = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/notifications-logo.html",
      query: { scenario: "native-activation", theme: "dark" },
    });
    expect(activation.crashes).toEqual([]);
    expect(activation.unexpected).toEqual([]);
    const activationCheck = activation.results.find((r) =>
      r.name.includes("NativeNotificationBridgeView renders current GitPulse SVG logo"),
    );
    expect(activationCheck?.pass, JSON.stringify(activation.results)).toBe(true);
    expect(activation.data?.surfaces).toMatchObject({
      native: { mounted: true, hasGitPulseLogo: true, visibleBox: true },
    });

    const errBanner = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/notifications-logo.html",
      query: { scenario: "native-error", theme: "dark" },
    });
    expect(errBanner.crashes).toEqual([]);
    expect(errBanner.unexpected).toEqual([]);
    const errCheck = errBanner.results.find((r) =>
      r.name.includes("NativeNotificationBridgeView renders current GitPulse SVG logo"),
    );
    expect(errCheck?.pass, JSON.stringify(errBanner.results)).toBe(true);
    expect(errBanner.data?.surfaces).toMatchObject({
      native: { mounted: true, hasGitPulseLogo: true, visibleBox: true },
    });
  });

  it("renders current GitPulse Logo in SessionNotificationBridge missed-session status banner in Chrome", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/notifications-logo.html",
      query: { scenario: "session-missed", theme: "dark" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    const sessionCheck = verdict.results.find((r) =>
      r.name.includes("SessionNotificationBridge renders current GitPulse SVG logo"),
    );
    expect(sessionCheck?.pass, JSON.stringify(verdict.results)).toBe(true);
    expect(verdict.data?.surfaces).toMatchObject({
      session: { mounted: true, hasGitPulseLogo: true, visibleBox: true },
    });
  });

  it("SSR renders AttentionInbox with current GitPulse Logo and intact filter/refresh controls", () => {
    const { body } = render(AttentionInbox, {
      props: { scope: { kind: "global" }, active: false, onopen: async () => {} },
    });

    expect(body).toContain('aria-label="Inbox"');
    expect(body).toContain(CANONICAL_PATH_FRAGMENT);
    expect(body).toContain('aria-label="Waylode"');
    expect(body).toContain('aria-label="Inbox filter"');
    expect(body).toContain(">Refresh</button>");
  });

  it("renders visible non-zero Logo SVGs across all 4 notification surfaces in dark theme in Chrome", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/notifications-logo.html",
      query: { scenario: "all", theme: "dark" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.total).toBeGreaterThanOrEqual(8);
    expect(verdict.passed).toBe(verdict.total);
  });

  it("renders visible non-zero Logo SVGs across all 4 notification surfaces in light theme in Chrome", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/notifications-logo.html",
      query: { scenario: "all", theme: "light" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.total).toBeGreaterThanOrEqual(8);
    expect(verdict.passed).toBe(verdict.total);
  });
});

// ---------------------------------------------------------------------------
// F4 — Menu Bar Tray Glyph & StatusPopover Logo (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 1] [R1] [R2] F4 — Menu Bar Tray Glyph & StatusPopover Logo", () => {
  it(
    "passes Rust desktop::tray unit tests",
    async () => {
      const res = await execCargoSuite(["--lib", "desktop::tray"], {
        timeoutMs: 280_000,
      });
      assertCargoPassed(res, "cargo test --lib desktop::tray");
    },
    300_000,
  );

  it("updates tray.rs pulse_icon to draw the G-ring + heartbeat glyph instead of the legacy 6-point pulse polyline", () => {
    const trayRs = readRepoFile("src-tauri/src/desktop/tray.rs");
    assertTrayPulseIconUpdated(trayRs);
  });

  it("SSR renders StatusPopover header .mark with current GitPulse Logo rather than Lucide Activity", () => {
    const snapshot = makeMenuState();
    const { body } = render(StatusPopover, {
      props: { snapshot, onaction: () => {} },
    });

    const markMatch = /<div class="mark\b[^>]*>([\s\S]*?)<\/div>/.exec(body);
    expect(markMatch).not.toBeNull();
    const markHtml = markMatch![1]!;

    expect(markHtml).toContain(CANONICAL_PATH_FRAGMENT);
    expect(markHtml).not.toContain("lucide-activity");
  });

  it("SSR renders StatusPopover connecting/empty state (snapshot = null) with GitPulse Logo rather than Lucide Activity", () => {
    const { body } = render(StatusPopover, {
      props: { snapshot: null, onaction: () => {} },
    });

    expect(body).toContain("Connecting");
    expect(body).toContain(CANONICAL_PATH_FRAGMENT);
    expect(body).not.toContain("lucide-activity");
  });

  it("renders visible non-zero GitPulse Logo SVG inside .mark in Chrome (status-popover.html?scenario=waiting&theme=dark)", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/status-popover.html",
      query: { scenario: "waiting", theme: "dark" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    const markCheck = verdict.results.find((r) =>
      r.name.includes("header .mark with current GitPulse SVG logo"),
    );
    expect(markCheck?.pass, JSON.stringify(verdict.results)).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// F5 — Overview Direct Conflicts & Resolve Visibility (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 1] [R3] F5 — Overview Direct Conflicts & Resolve Visibility", () => {
  it("workConflicts openConflicts() sets conflictsFocusRequest, switches repoStore to (work, overview), and consumes once", () => {
    const navigate = vi.spyOn(repoStore, "setActiveTab").mockImplementation(() => {});
    openConflicts();

    expect(navigate).toHaveBeenCalledWith("work", "overview");
    expect(get(conflictsFocusRequest)).toBe(true);
    expect(takeConflictsReveal()).toBe(true);
    expect(takeConflictsReveal()).toBe(false);
    expect(takeConflictsFocus()).toBe(true);
    expect(takeConflictsFocus()).toBe(false);
    expect(get(conflictsFocusRequest)).toBe(false);
  });

  it("SSR renders WorkView in a clean open repo with an accessible enabled Conflicts/Resolve control", async () => {
    await withMockedOpenRepo("clean", () => {
      const { body } = render(WorkView);
      expect(body).toContain('data-testid="overview-conflicts-btn"');
      expect(body).toContain('data-testid="overview-repo-conflicts-btn"');

      const headerBtn = body.match(/<button[^>]*data-testid="overview-conflicts-btn"[^>]*>/)?.[0];
      expect(headerBtn).toBeDefined();
      expect(headerBtn).not.toContain("disabled");

      const repoBtn = body.match(
        /<button[^>]*data-testid="overview-repo-conflicts-btn"[^>]*>/,
      )?.[0];
      expect(repoBtn).toBeDefined();
      expect(repoBtn).not.toContain("disabled");
    });
  });

  it("SSR renders WorkView in a dirty repo with Conflicts/Resolve exposed alongside Review changes", async () => {
    await withMockedOpenRepo("dirty", () => {
      const { body } = render(WorkView);
      expect(body).toContain("Review changes");
      expect(body).toContain('data-testid="overview-conflicts-btn"');
      expect(body).toContain('data-testid="overview-repo-conflicts-btn"');
      expect(body).toContain("Resolve conflicts");
    });
  });

  it("SSR renders WorkOverviewPane Overview section and ConflictEditor clean empty state", () => {
    const paneHtml = render(WorkOverviewPane, {
      props: { loadConflict: () => new Promise<never>(() => {}) },
    }).body;
    expect(paneHtml).toContain('aria-label="Overview"');
    expect(paneHtml).toContain(`data-overview-layout="overview"`);

    const editorHtml = render(ConflictEditor).body;
    expect(editorHtml).toMatch(/<h2\b[^>]*>Resolve conflicts<\/h2>/);
    expect(editorHtml).toContain("No merge conflicts");
    expect(editorHtml).toContain("No conflicted files are listed in this repository.");
  });

  it("exposes work:conflicts in Command Palette catalog without disabling it when a repository is open and clean", () => {
    const cleanState = {
      ...get(repoStore),
      activeTabId: "tab-1",
      currentPath: "/workspace/GitPulse",
      currentBranch: "main",
      isLoading: false,
      isBare: false,
      statuses: [],
      operation: { operation: null, probeFailed: false },
    };
    const commands = buildCommands(cleanState, () => {});
    const conflictsCmd = commands.find((c) => c.id === "work:conflicts");
    expect(conflictsCmd).toBeDefined();
    expect(conflictsCmd?.disabledReason).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// F6 — Overview Conflicts Open/Hide Lifecycle & Live Priority (5 tests)
// ---------------------------------------------------------------------------
describe("[Tier 1] [R3] F6 — Overview Conflicts Open/Hide Lifecycle & Live Priority", () => {
  it("opens, focuses, and hides #work-conflicts cleanly in a clean repo in Chrome", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "clean" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed).toBe(verdict.total);
    expect(verdict.data?.activeElementId).toBe(WORK_CONFLICTS_ID);
    expect(verdict.data?.layoutAfterOpen).toBe("with-conflicts");
    expect(verdict.data?.layoutAfterClose).toBe("overview");
  });

  it("opens, focuses, and hides #work-conflicts cleanly in a dirty repo in Chrome", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "dirty" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed).toBe(verdict.total);
    expect(verdict.data?.activeElementId).toBe(WORK_CONFLICTS_ID);
    expect(verdict.data?.layoutAfterClose).toBe("overview");
  });

  it("opens #work-conflicts from Overview with retained drafts and collapses cleanly on Hide in Chrome", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "drafts" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed).toBe(verdict.total);
    expect(verdict.data?.layoutAfterOpen).toBe("with-conflicts");
    expect(verdict.data?.layoutAfterClose).toBe("overview");
  });

  it("leads Overview automatically with #work-conflicts when conflicted in Chrome", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "conflicted" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed).toBe(verdict.total);
    expect(verdict.data?.layoutBefore).toBe("with-conflicts");
  });

  it("preserves WorkView DOM node identity across clean -> open -> hide -> conflicted -> resolved transitions", async () => {
    const verdict = await runBrowserPage({
      page: "/tests/e2e-r1-r4/browser/overview-conflicts.html",
      query: { scenario: "transition" },
    });
    expect(verdict.crashes).toEqual([]);
    expect(verdict.unexpected).toEqual([]);
    expect(verdict.passed).toBe(verdict.total);
    expect(verdict.data?.workViewPreserved).toBe(true);
  });
});
