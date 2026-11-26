import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import {
  DEFAULT_IMAGE,
  buildContainerScript,
  buildPodmanRunArgs,
  main,
  parseArgs,
  shellQuote,
  volumeMounts,
} from "./linux-podman.mjs";

describe("linux-podman harness", () => {
  it("answers --help with exit 0 and refuses invalid modes or flags with exit 2", () => {
    const logged: string[] = [];
    const origLog = console.log;
    const origErr = console.error;
    console.log = (...a: unknown[]) => void logged.push(a.join(" "));
    console.error = () => {};
    try {
      expect(main(["--help"])).toBe(0);
      expect(logged.join("\n")).toContain("Usage:");
      expect(main(["not-a-mode"])).toBe(2);
      expect(main(["webview", "--display", "mir"])).toBe(2);
      expect(main(["rust-test", "--shard", "bad"])).toBe(2);
      expect(main(["rust-test", "--shard", "0/3"])).toBe(2);
      expect(main(["rust-test", "--shard", "4/3"])).toBe(2);
    } finally {
      console.log = origLog;
      console.error = origErr;
    }
  });

  it("mounts named volumes for target, cargo, node_modules, dist, and bun cache so host files stay untouched", () => {
    const mounts = volumeMounts("gitpulse-linux");
    expect(mounts).toContain("gitpulse-linux-target:/cargo-target");
    expect(mounts).toContain("gitpulse-linux-cargo:/cargo-home");
    expect(mounts).toContain("gitpulse-linux-node-modules:/workspace/node_modules");
    expect(mounts).toContain("gitpulse-linux-dist:/workspace/dist");
    expect(mounts).toContain("gitpulse-linux-bun-cache:/bun-cache");

    const opts = parseArgs(["e2e", "--display", "wayland", "--platform", "linux/amd64"], "/repo");
    const args = buildPodmanRunArgs(opts, "echo hi", "/repo");
    expect(args).toContain("--init");
    expect(args).toContain("--cap-drop=DAC_OVERRIDE");
    expect(args).toContain("--cap-drop=DAC_READ_SEARCH");
    expect(args.some((a) => /^CARGO_BUILD_JOBS=\d+$/.test(a))).toBe(true);
    expect(args).toContain("--platform");
    expect(args).toContain("linux/amd64");
    expect(args).toContain("gitpulse-linux-amd64-target:/cargo-target");
    expect(args.some((a) => a.endsWith(":/e2e-artifacts"))).toBe(true);
    expect(args).toContain(DEFAULT_IMAGE);
  });

  it("generates accurate X11 and Wayland commands for gtk-runtime, webview, e2e, and sharded rust-test", () => {
    const gtk = buildContainerScript(parseArgs(["gtk-runtime"]), "wayland");
    expect(gtk).toContain("with-display wayland cargo test --manifest-path src-tauri/Cargo.toml --locked --test gtk_runtime");

    const webview = buildContainerScript(parseArgs(["webview", "--harness", "terminal"]), "x11");
    expect(webview).toContain("cargo build --manifest-path src-tauri/Cargo.toml --locked --example webview_regressions");
    expect(webview).toContain("/workspace/public");
    expect(webview).toContain("ln -s /workspace/src-tauri/src /tmp/gp-web/src-tauri/src");
    expect(webview).toContain("WEBKIT_DISABLE_DMABUF_RENDERER=");
    expect(webview).toContain("WEBKIT_DMABUF_RENDERER_FORCE_SHM=");
    expect(webview).toContain("WEBKIT_SKIA_ENABLE_CPU_RENDERING=");
    expect(webview).toContain("GITPULSE_VITE_CACHE_DIR=/tmp/gp-web/.vite-cache");
    expect(webview).toContain("with-display x11 node scripts/browser-regressions.mjs --native --harness 'terminal'");

    const webviewAll = buildContainerScript(parseArgs(["webview", "--all"]), "wayland");
    expect(webviewAll).toContain("--native --all");

    const e2e = buildContainerScript(parseArgs(["e2e"]), "wayland");
    expect(e2e).toContain("--features tauri/custom-protocol --bin waylode --bin waylode-ptyd --example e2e_seed_trust");
    expect(e2e).toContain("with-display wayland node scripts/e2e-driver-smoke.mjs --artifacts /e2e-artifacts/linux-wayland");

    const test = buildContainerScript(parseArgs(["rust-test", "--lib", "--test", "git_environment_pins", "--shard", "2/3", "--filter", "test(foo)"]), "x11");
    expect(test).toContain("--lib");
    expect(test).toContain("--test 'git_environment_pins'");
    expect(test).toContain("--partition 'hash:2/3'");
    expect(test).toContain("-E 'test(foo)'");
    expect(shellQuote("a'b")).toBe("'a'\\''b'");
  });

  it("keeps Containerfile.linux-test pinned to Ubuntu 22.04, Bun, nextest, devmap, and manvi versions matching the repo", () => {
    const cf = readFileSync(new URL("../Containerfile.linux-test", import.meta.url), "utf8");
    const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
    const ci = readFileSync(new URL("../.github/workflows/ci.yml", import.meta.url), "utf8");
    const bunVersion = String(pkg.packageManager).replace(/^bun@/, "");
    const nextestVersion = /cargo-nextest@(\d+\.\d+\.\d+)/.exec(ci)?.[1];
    const devmapRev = /^\s*DEVMAP_REV:\s*([0-9a-f]{40})\b/m.exec(ci)?.[1];
    const manviRev = /^\s*MANVI_REV:\s*([0-9a-f]{40})\b/m.exec(ci)?.[1];
    expect(cf).toContain("FROM ubuntu:22.04");
    expect(cf).toContain(`ARG BUN_VERSION=${bunVersion}`);
    expect(nextestVersion).toBeTruthy();
    expect(cf).toContain(`ARG NEXTEST_VERSION=${nextestVersion}`);
    expect(devmapRev).toBeTruthy();
    expect(cf).toContain(`ARG DEVMAP_REV=${devmapRev}`);
    expect(manviRev).toBeTruthy();
    expect(cf).toContain(`ARG MANVI_REV=${manviRev}`);
    for (const tool of ["libwebkit2gtk-4.1-dev", "webkit2gtk-driver", "xvfb", "weston", "dbus-x11", "tauri-driver", "openssh-client", "lsof", "sqlite3", "git-core/ppa", "worktree list --porcelain -z", "devmap --version", "manvi --version"]) {
      expect(cf).toContain(tool);
    }
    expect(cf).not.toMatch(/curl[^|\n]*\|\s*(?:ba)?sh/);
  });
});
