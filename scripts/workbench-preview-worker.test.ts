import { execFile, spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { mkdtemp, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

describe("disposable Manvi preview lifecycle", () => {
  it("announces the listen URL without stdio buffering", () => {
    expect(readFileSync(new URL("./workbench-preview.mjs", import.meta.url), "utf8")).toContain("writeSync(1,");
  });
  it.skipIf(process.platform === "win32")("owns SIGTERM cleanup after Vite begins listening", async () => {
    const fixture = await mkdtemp(join(tmpdir(), "gitpulse-preview-signal-"));
    const store = join(fixture, "dcstore");
    const host = join(fixture, "manvi");
    // This test exercises server disposal only; real-store workflows use the
    // browser integration harness and explicit native integration test.
    await writeFile(store, '#!/usr/bin/env node\nconsole.log(JSON.stringify({ok:true}));\n', { mode: 0o700 });
    await writeFile(host, `#!/usr/bin/env node
const { createInterface } = require('node:readline');
const lines = createInterface({input:process.stdin});
lines.on('line', line => { const request = JSON.parse(line); console.log(JSON.stringify({id:request.id,ok:true,result:{ops:['work.enhancements.generate']}})); });
lines.on('close', () => setTimeout(() => process.exit(0), 250));
`, { mode: 0o700 });
    const script = fileURLToPath(new URL("./workbench-preview.mjs", import.meta.url));
    const child = spawn(process.execPath, [script, store, host], { stdio: ["ignore", "pipe", "pipe"] });
    let output = "", diagnostics = "", root = "";
    child.stderr.on("data", (chunk: Buffer) => { diagnostics = (diagnostics + chunk.toString()).slice(-4096); });
    const done = new Promise<number | null>((resolve) => child.once("exit", resolve));
    try {
      await new Promise<void>((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error(`Preview did not listen: ${diagnostics}\nstdout: ${output}`)), 30_000);
        child.once("error", (error) => { clearTimeout(timeout); reject(error); });
        child.stdout.on("data", (chunk: Buffer) => {
          output += chunk.toString();
          if (output.includes("Preview: http://")) { clearTimeout(timeout); resolve(); }
        });
      });
      root = output.split("Disposable fixture directory: ")[1]?.split("\n")[0] ?? "";
      expect(root).not.toBe("");
      child.kill("SIGTERM");
      const timeout = setTimeout(() => child.kill("SIGKILL"), 3000);
      const code = await done;
      clearTimeout(timeout);
      expect(code, diagnostics).toBe(0);
      await expect(stat(root)).rejects.toMatchObject({ code: "ENOENT" });
    } finally {
      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGTERM");
        const kill = setTimeout(() => child.kill("SIGKILL"), 3000);
        await done;
        clearTimeout(kill);
      }
      if (root) await rm(root, { recursive: true, force: true });
      await rm(fixture, { recursive: true, force: true });
    }
  }, 45_000);
  it("removes the disposable profile when the host cannot start", async () => {
    const script = fileURLToPath(new URL("./workbench-preview.mjs", import.meta.url));
    let output = "";
    try {
      await promisify(execFile)(process.execPath, [script, process.execPath, process.execPath], { timeout: 3000, maxBuffer: 32768 });
      throw new Error("Unexpected successful startup");
    } catch (error) {
      if (!(error instanceof Error) || !("stdout" in error) || typeof error.stdout !== "string" || !("code" in error) || error.code !== 1) throw error;
      output = error.stdout;
    }
    const root = output.split("Disposable fixture directory: ")[1]?.split("\n")[0];
    expect(root).toBeTruthy();
    if (!root) throw new Error("Missing fixture directory");
    await expect(stat(root)).rejects.toMatchObject({ code: "ENOENT" });
  });
  it.each(["missing", "exits"])("releases the loopback server when the host %s before hello", async (failure) => {
    const root = await mkdtemp(join(tmpdir(), "gitpulse-preview-lifecycle-"));
    try {
      const module = new URL("./workbench-preview-worker.mjs", import.meta.url).href;
      const binary = failure === "missing" ? join(root, "not-installed") : process.execPath;
      const script = `
        import { previewWorker } from ${JSON.stringify(module)};
        try {
          await previewWorker(${JSON.stringify(binary)}, 'unused', ${JSON.stringify(root)}, ${JSON.stringify(join(root, "profile.sqlite"))});
          throw new Error('Unexpected successful handshake');
        } catch (error) {
          if (error.message === 'Unexpected successful handshake') throw error;
          console.log('Startup failed and all handles closed');
        }
      `;
      // Do not force exit: an abandoned model listener makes this time out.
      const result = await promisify(execFile)(process.execPath, ["--input-type=module", "-e", script], { timeout: 3000, maxBuffer: 4096 });
      expect(result.stdout.trim()).toBe("Startup failed and all handles closed");
      expect(result.stderr).toBe("");
    } finally { await rm(root, { recursive: true, force: true }); }
  });
});
