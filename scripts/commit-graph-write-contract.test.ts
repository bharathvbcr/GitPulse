import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { readCommandsSource } from "./commands-source";

/**
 * Owner Decision 3: the commit-graph write is a user-clicked, gated, previewed
 * action and nothing else. These pin the reachability, from both sides of the
 * IPC boundary: no background, MCP or read path can run it, and the one
 * command is invoked from the one confirm click.
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

const WRITE = /cmd_commit_graph_write(?!_plan|_cancel)\b/;

describe("the frontend reaches the write from one place", () => {
  const frontend = sources("src", (name) => /\.(ts|svelte)$/.test(name) && !isTest(name));

  it("only the write flow invokes cmd_commit_graph_write, and only the component builds the flow", () => {
    const code = (file: string) => read(file).replace(/\/\*[\s\S]*?\*\/|<!--[\s\S]*?-->|\/\/.*/g, "");
    const invokers = frontend.filter((file) => WRITE.test(code(file)));
    expect(invokers.map((file) => relative("src", file))).toEqual(["lib/health/commitGraphWrite.ts"]);
    const builders = frontend.filter((file) => read(file).includes("createCommitGraphWriteFlow("));
    expect(builders.map((file) => relative("src", file)).sort()).toEqual([
      "lib/components/CommitGraphWrite.svelte",
      "lib/health/commitGraphWrite.ts",
    ]);
  });

  it("the write command is not a shared read, so nothing prefetches or replays it", () => {
    const shared = read("src/lib/ipc/sharedReads.ts");
    expect(shared).not.toContain("cmd_commit_graph_write");
  });

  it("the flow invokes the write from confirm, behind the confirming phase, with the previewed argv", () => {
    const flow = read("src/lib/health/commitGraphWrite.ts");
    const confirm = flow.slice(flow.indexOf("async confirm()"), flow.indexOf("async cancel()"));
    expect(confirm).toContain('if (step.phase !== "confirming") return;');
    expect(confirm).toContain("confirmedArgv: plan.argv");
    expect(flow.match(/"cmd_commit_graph_write"/g)).toHaveLength(1);
    expect(flow).not.toMatch(/setTimeout|setInterval|\$effect|onMount/);
  });

  it("the component calls confirm from exactly one click handler: the confirm button", () => {
    const component = read("src/lib/components/CommitGraphWrite.svelte");
    expect(component.match(/flow\.confirm\(\)/g)).toHaveLength(1);
    const at = component.indexOf("flow.confirm()");
    expect(component.slice(component.lastIndexOf("<button", at), at)).toContain(
      'data-testid="commit-graph-write-confirm"',
    );
    expect(component).not.toMatch(/onMount|setTimeout|setInterval/);
    // The one effect follows the scan; it cannot start, confirm or open a write.
    const effects = [...component.matchAll(/\$effect\(\(\) => \{([\s\S]*?)\n  \}\);/g)];
    expect(effects).toHaveLength(1);
    expect(effects[0][1].trim()).toBe("flow.syncInFlight(!!report?.write_in_flight);");
  });

  it("the preview shows no speedup figure and says what the click leaves behind", () => {
    const component = read("src/lib/components/CommitGraphWrite.svelte");
    const flow = read("src/lib/health/commitGraphWrite.ts");
    for (const ui of [component, flow]) {
      expect(ui).not.toMatch(/\b\d+(\.\d+)?\s?[x×]\b|times faster|speed-?up|faster than/i);
    }
    expect(component).toContain("future commit-graph writes will");
    expect(component).toContain("--no-changed-paths");
  });

  it("the panel mounts the component and never invokes the command itself", () => {
    const panel = read("src/lib/components/StoragePanel.svelte");
    expect(panel).toContain("<CommitGraphWrite");
    expect(panel).not.toMatch(/cmd_commit_graph_write/);
  });
});

describe("the backend exposes the write to the webview only", () => {
  it("no MCP source, snapshot or resource names it", () => {
    for (const file of sources("src-tauri/src/mcp", () => true)) {
      expect(read(file), `${file} must not reach the commit-graph writer`).not.toMatch(
        /commit_graph_write|commit-graph write/,
      );
    }
  });

  it("neither ingest, hooks, the timers nor the harness name it", () => {
    for (const dir of ["ingest", "hooks", "harness", "terminal", "alerts", "insights"]) {
      for (const file of sources(`src-tauri/src/${dir}`, (name) => name.endsWith(".rs") && !isTest(name))) {
        expect(read(file), `${file} must not reach the commit-graph writer`).not.toMatch(
          /commit_graph_write|CommitGraphWrite/,
        );
      }
    }
  });

  const COMMANDS = readCommandsSource();

  it("the write command preflights, builds one argv, judges it, then runs that argv", () => {
    const body = fnBody(COMMANDS, "cmd_commit_graph_write");
    const order = [
      "GitWriter::commit_graph_write_plan(",
      "GitWriter::commit_graph_write_argv()",
      "guard(&repo_path",
      "GitWriter::commit_graph_write(&repo_path, &argv)",
    ].map((needle) => body.indexOf(needle));
    expect(order.every((index) => index >= 0), `missing step in ${order}`).toBe(true);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    expect(body).not.toMatch(/"git"|guard_background/);
  });

  it("the writer and its argv name no git config write", () => {
    const writer = read("src-tauri/src/engine/git_writer.rs");
    const argv = /COMMIT_GRAPH_WRITE_ARGV: \[&str; \d+\] = \[([^\]]*)\]/.exec(writer);
    expect(argv?.[1].replace(/\s+/g, "")).toBe(
      '"git","commit-graph","write","--reachable","--changed-paths",',
    );
    for (const name of [
      "commit_graph_write",
      "commit_graph_write_checked",
      "commit_graph_write_within",
      "commit_graph_write_run",
      "commit_graph_write_plan",
      "commit_graph_write_plan_with",
      "commit_graph_write_refusal",
    ]) {
      expect(fnBody(writer, name), `${name} must not touch git config`).not.toMatch(
        /"config"|core\.commitGraph|gc\.writeCommitGraph|set_identity/,
      );
    }
  });

  it("the writer never deletes a lock file", () => {
    const writer = read("src-tauri/src/engine/git_writer.rs");
    const start = writer.indexOf("const COMMIT_GRAPH_LOCKS");
    const end = writer.indexOf("fn commit_graph_lock_remedy");
    expect(start).toBeGreaterThan(0);
    expect(end).toBeGreaterThan(start);
    expect(writer.slice(start, end + 500)).not.toMatch(/remove_file|remove_dir/);
    expect(fnBody(writer, "commit_graph_write_run")).not.toMatch(/remove_file|remove_dir/);
  });

  it("the write runs with lazy fetch off, and a partial clone is refused first", () => {
    const writer = read("src-tauri/src/engine/git_writer.rs");
    expect(fnBody(writer, "commit_graph_write_run")).toContain("without_lazy_fetch(");
    expect(fnBody(writer, "commit_graph_write_refusal")).toContain("is_partial_clone(");
    expect(fnBody(writer, "commit_graph_write_plan_with")).toContain("commit_graph_write_refusal(");
    expect(fnBody(writer, "commit_graph_write_checked")).toContain("commit_graph_write_refusal(");
  });
});
