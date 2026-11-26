import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A test that runs a real Tauri event loop on macOS must not take over the
 * person's screen.
 *
 * Tao launches with `ActivationPolicy::Regular` (a Dock icon and a menu bar) and
 * `activate_ignoring_other_apps: true` unless the application says otherwise,
 * and it applies that when the loop starts, whether or not the test's windows
 * are `visible(false)`. `tests/common/quiet_app.rs` sets `Accessory` between
 * `build` and `run`. This asserts that every target which runs a real loop
 * calls it, so a new native test cannot put an icon in the Dock again.
 */
const TESTS = fileURLToPath(new URL("../src-tauri/tests", import.meta.url));
const HELPER = path.join(TESTS, "common", "quiet_app.rs");

/** `Builder::default()` that is `run`, as opposed to a mock runtime that only builds. */
export function runsARealLoop(source: string): boolean {
  return /tauri::Builder::default\(\)/.test(source) && /\bapp\.run\(/.test(source);
}

export function silencesTheApp(source: string): boolean {
  return (
    /#\[path = "common\/quiet_app\.rs"\]\s*mod quiet_app;/.test(source) &&
    // Launch without activating: only the builder carries that switch.
    /quiet_app::background_launch\(tauri::Builder::default\(\)\)/.test(source) &&
    /quiet_app::out_of_the_way\(&mut app\)/.test(source) &&
    // Between build and run: setting it after `run` returns would never happen.
    source.indexOf("out_of_the_way(&mut app)") < source.search(/\bapp\.run\(/)
  );
}

const targets = readdirSync(TESTS)
  .filter((name) => name.endsWith(".rs"))
  .map((name) => ({ name, source: readFileSync(path.join(TESTS, name), "utf8") }))
  // gtk_runtime runs a real loop on Linux, where there is no activation policy.
  .filter(({ name, source }) => runsARealLoop(source) && source.includes("target_os = \"macos\"") && name !== "gtk_runtime.rs");

describe("native test quiet contract", () => {
  it("finds the native targets it is meant to guard", () => {
    const names = targets.map((t) => t.name);
    for (const expected of ["browser_pane_isolation.rs", "native_quit_guard.rs", "native_status_material.rs"]) {
      expect(names, `${expected} no longer runs a real loop, or the detector stopped seeing it`).toContain(expected);
    }
  });

  for (const { name, source } of targets) {
    it(`${name} runs its loop as an Accessory application`, () => {
      expect(silencesTheApp(source), `${name} runs a real Tauri loop without quiet_app::out_of_the_way`).toBe(true);
    });
  }

  it("the helper sets Accessory, which has no Dock icon", () => {
    expect(readFileSync(HELPER, "utf8")).toContain("tauri::ActivationPolicy::Accessory");
  });

  it("the helper keeps reveals off the screen", () => {
    // Production paths the tests drive (a quit confirmation, an agent's browse
    // request) call reveal_main, which would show and focus the window and put
    // the Dock icon back.
    const helper = readFileSync(HELPER, "utf8");
    const body = helper.slice(helper.indexOf("pub fn out_of_the_way"));
    expect(body.slice(0, body.indexOf("\n}"))).toContain("waylode_lib::desktop::keep_off_screen();");
  });

  it("the detector rejects the shape before the fix and accepts it after", () => {
    const before = `fn main() {\n let app = tauri::Builder::default().build(ctx).unwrap();\n app.run(|_, _| {});\n}`;
    const after = `#[path = "common/quiet_app.rs"]\nmod quiet_app;\nfn main() {\n let mut app = quiet_app::background_launch(tauri::Builder::default()).build(ctx).unwrap();\n quiet_app::out_of_the_way(&mut app);\n app.run(|_, _| {});\n}`;
    const afterRun = `#[path = "common/quiet_app.rs"]\nmod quiet_app;\nfn main() {\n let mut app = quiet_app::background_launch(tauri::Builder::default()).build(ctx).unwrap();\n app.run(|_, _| {});\n quiet_app::out_of_the_way(&mut app);\n}`;
    const activating = `#[path = "common/quiet_app.rs"]\nmod quiet_app;\nfn main() {\n let mut app = tauri::Builder::default().build(ctx).unwrap();\n quiet_app::out_of_the_way(&mut app);\n app.run(|_, _| {});\n}`;
    expect(runsARealLoop(before)).toBe(true);
    expect(silencesTheApp(before)).toBe(false);
    expect(silencesTheApp(after)).toBe(true);
    expect(silencesTheApp(afterRun)).toBe(false);
    expect(silencesTheApp(activating)).toBe(false);
    // A mock runtime that never runs the loop is not a target.
    expect(runsARealLoop(`tauri::test::mock_builder().build(ctx)`)).toBe(false);
  });
});
