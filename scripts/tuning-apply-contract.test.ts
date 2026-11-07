import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { readCommandsSource } from "./commands-source";

/**
 * Owner decision 2026-11-06 (ft-6b68be32): recommended git tuning may be
 * applied, but only by a user-clicked, previewed, gated action and nothing
 * else. These pin the reachability from both sides of the IPC boundary, the
 * way commit-graph-write-contract.test.ts does for the graph write: no
 * background, MCP or read path can run it, and the one command is invoked from
 * the one confirm click. The Rust side (`tests/tuning_apply_reach.rs`) pins the
 * same over the Rust source.
 */
const ROOT = fileURLToPath(new URL("..", import.meta.url));
const read = (path: string) => readFileSync(join(ROOT, path), "utf8");

function sources(dir: string, accept: (name: string) => boolean): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(join(ROOT, dir))) {
    const rel = join(dir, entry);
    if (statSync(join(ROOT, rel)).isDirectory()) found.push(...sources(rel, accept));
    else if (accept(entry)) found.push(rel);
  }
  return found;
}

const isTest = (name: string) => /\.(test|spec)\.ts$/.test(name) || /tests?\.rs$/.test(name);

function fnBody(source: string, name: string): string {
  const signature = new RegExp(`fn ${name}\\s*[(<]`).exec(source);
  expect(signature, `fn ${name} must exist`).not.toBeNull();
  const open = source.indexOf("{", signature!.index);
  let depth = 0;
  for (let cursor = open; cursor < source.length; cursor += 1) {
    if (source[cursor] === "{") depth += 1;
    else if (source[cursor] === "}") {
      depth -= 1;
      if (depth === 0) return source.slice(open, cursor + 1);
    }
  }
  throw new Error(`unbalanced braces in ${name}`);
}

const APPLY = /cmd_tuning_apply(?!_cancel)\b/;
const strip = (source: string) => source.replace(/\/\*[\s\S]*?\*\/|<!--[\s\S]*?-->|\/\/.*/g, "");

describe("the frontend reaches the tuning apply from one place", () => {
  const frontend = sources("src", (name) => /\.(ts|svelte)$/.test(name) && !isTest(name));

  it("only the tuning flow invokes cmd_tuning_apply, and only the component builds the flow", () => {
    const invokers = frontend.filter((file) => APPLY.test(strip(read(file))));
    expect(invokers.map((file) => relative("src", file).split(sep).join("/"))).toEqual(["lib/health/tuningApply.ts"]);
    const builders = frontend.filter((file) => read(file).includes("createTuningApplyFlow("));
    expect(builders.map((file) => relative("src", file).split(sep).join("/")).sort()).toEqual([
      "lib/components/TuningApply.svelte",
      "lib/health/tuningApply.ts",
    ]);
  });

  it("the apply command is not a shared read, so nothing prefetches or replays it", () => {
    expect(read("src/lib/ipc/sharedReads.ts")).not.toContain("cmd_tuning");
  });

  it("the flow applies from confirm, behind the confirming phase, with the previewed argv", () => {
    const flow = read("src/lib/health/tuningApply.ts");
    const confirm = flow.slice(flow.indexOf("async confirm()"), flow.indexOf("async cancel()"));
    expect(confirm).toContain('if (step.phase !== "confirming") return;');
    expect(confirm).toContain("confirmedArgv: runnableArgv(plan)");
    expect(flow.match(/"cmd_tuning_apply"/g)).toHaveLength(1);
    expect(strip(flow)).not.toMatch(/setTimeout|setInterval|\$effect|onMount/);
  });

  it("the component calls confirm from exactly one click handler: the confirm button", () => {
    const component = read("src/lib/components/TuningApply.svelte");
    expect(component.match(/flow\.confirm\(\)/g)).toHaveLength(1);
    const at = component.indexOf("flow.confirm()");
    expect(component.slice(component.lastIndexOf("<button", at), at)).toContain('data-testid="tuning-apply-confirm"');
    expect(component).not.toMatch(/onMount|setTimeout|setInterval/);
    // The one effect follows the scan; it cannot start, confirm or open an apply.
    const effects = [...component.matchAll(/\$effect\(\(\) => \{([\s\S]*?)\n  \}\);/g)];
    expect(effects).toHaveLength(1);
    expect(effects[0][1].trim()).toBe("flow.syncInFlight(!!advice?.apply_in_flight);");
  });

  it("the preview names each argv, what it writes and where, and how to undo it", () => {
    const component = read("src/lib/components/TuningApply.svelte");
    for (const testid of [
      "tuning-step-argv",
      "tuning-step-write",
      "tuning-step-undo",
      "tuning-step-note",
      "tuning-step-undo-note",
    ]) {
      expect(component, testid).toContain(`data-testid="${testid}"`);
    }
    for (const scope of ["repo_config", "global_config", "os_scheduler"]) {
      expect(component, scope).toContain(scope);
    }
  });

  it("the panel mounts the component and never invokes the command itself", () => {
    const panel = read("src/lib/components/StoragePanel.svelte");
    expect(panel).toContain("<TuningApply");
    expect(panel).not.toMatch(/cmd_tuning/);
  });
});

describe("the backend exposes the apply to the webview only", () => {
  it("no MCP source, snapshot or resource names it", () => {
    for (const file of sources("src-tauri/src/mcp", () => true)) {
      expect(read(file), `${file} must not reach the tuning apply`).not.toMatch(/tuning_apply|cmd_tuning|repo_tuning|TuningChange/);
    }
  });

  it("neither ingest, hooks, the timers nor the harness name it", () => {
    for (const dir of ["ingest", "hooks", "harness", "terminal", "alerts", "insights", "watcher"]) {
      for (const file of sources(`src-tauri/src/${dir}`, (name) => name.endsWith(".rs") && !isTest(name))) {
        expect(read(file), `${file} must not reach the tuning apply`).not.toMatch(/tuning_apply|cmd_tuning|TuningChange/);
      }
    }
  });

  const COMMANDS = readCommandsSource();

  it("the apply command re-plans, compares the echo, judges every argv, then runs", () => {
    const body = fnBody(COMMANDS, "cmd_tuning_apply");
    const order = [
      "tuning_runnable(&repo_path",
      "confirmed_argv != runnable",
      "guard(&repo_path",
      "tuning_apply(&repo_path",
    ].map((needle) => body.indexOf(needle));
    expect(order.every((index) => index >= 0), `missing step in ${order}`).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    expect(body).not.toMatch(/"git"|guard_background/);
  });

  it("the executor refuses untrusted repositories, bounds the run and journals each step", () => {
    const executor = read("src-tauri/src/engine/tuning_apply.rs");
    expect(fnBody(executor, "plan_now")).toContain("repository_trust::require(");
    expect(executor).toContain("git_observed(");
    expect(executor).toContain("record_clicked_write(");
    expect(executor).toContain("flights().run(");
    expect(executor).toContain("is_apply_argv(");
    expect(executor).not.toMatch(/remove_file|remove_dir|Command::new/);
  });
});
