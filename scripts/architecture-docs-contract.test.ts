import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * The architecture docs listed "Tokio" beside "Rayon" as though both were
 * direct dependencies, so a contributor could reasonably write `use tokio::…`
 * and find it does not compile. Tokio is present only transitively through
 * Tauri.
 *
 * A prose claim about a manifest drifts the moment the manifest changes, so
 * this pins the two together: adding tokio as a direct dependency should fail
 * here and point at the sentence that then needs rewriting.
 */
const cargo = readFileSync(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8");
const architecture = readFileSync(new URL("../docs/ARCHITECTURE.md", import.meta.url), "utf8");
const readme = readFileSync(new URL("../README.md", import.meta.url), "utf8");

/** Crate names in the `[dependencies]` table only. */
function directDependencies(manifest: string): string[] {
  const start = manifest.indexOf("[dependencies]");
  expect(start, "[dependencies] must exist").toBeGreaterThanOrEqual(0);
  const rest = manifest.slice(start + "[dependencies]".length);
  const end = rest.search(/^\[/m);
  const table = end === -1 ? rest : rest.slice(0, end);
  return [...table.matchAll(/^([a-zA-Z0-9_-]+)\s*=/gm)].map((match) => match[1]);
}

describe("architecture documentation contract", () => {
  const deps = directDependencies(cargo);

  it("reads the dependency table it is asserting about", () => {
    expect(deps).toContain("tauri");
    expect(deps).toContain("rayon");
  });

  it("keeps the docs' 'tokio is not a direct dependency' claim true", () => {
    // If this fails because tokio was added deliberately, the fix is to update
    // the async-runtime note in ARCHITECTURE.md, not to delete this test.
    expect(deps).not.toContain("tokio");
    expect(architecture).toContain("Tokio is present, but transitively through Tauri");
  });

  it("does not name Tokio in the backend diagrams alongside a real dependency", () => {
    for (const [name, source] of [
      ["ARCHITECTURE.md", architecture],
      ["README.md", readme],
    ] as const) {
      const diagram = /subgraph Backend\["[^"]*"\]/.exec(source)?.[0] ?? "";
      expect(diagram, `${name} must have a backend subgraph`).not.toBe("");
      expect(diagram, `${name} diagram must not imply a direct Tokio dependency`).not.toContain(
        "Tokio",
      );
    }
  });

  it("points at the real off-thread mechanism", () => {
    expect(architecture).toContain("tauri::async_runtime::spawn_blocking");
    const commands = readFileSync(
      new URL("../src-tauri/src/commands/mod.rs", import.meta.url),
      "utf8",
    );
    // The doc names `off_thread`; it must still exist and use that runtime.
    expect(commands).toContain("async fn off_thread");
    expect(commands).toContain("tauri::async_runtime::spawn_blocking");
  });
});
