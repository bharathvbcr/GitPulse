import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The bundled app must contain the GUI, not one of the other binaries.
 *
 * `src/bin/*.rs` is auto-discovered by cargo, so adding `gitpulsed` gave the
 * package three binaries and the Tauri bundler stopped picking the right one:
 * `GitPulse.app` shipped the 3 MB headless attribution daemon as its
 * executable. That app launches, finds no repository named on its command
 * line, and exits — an installed application that opens nothing.
 *
 * Nothing caught it. `cargo test`, `clippy` and `ci:local` all passed, because
 * every binary was individually fine; only the *choice* of which one to bundle
 * was wrong, and that choice is made by a tool none of those run. It was found
 * by reading the bundle after the build claimed success.
 *
 * So this pins the two things that make the choice deterministic rather than a
 * function of cargo's target ordering.
 */
const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CARGO = readFileSync(path.join(REPO, "src-tauri", "Cargo.toml"), "utf8");
const TAURI_CONF = JSON.parse(readFileSync(path.join(REPO, "src-tauri", "tauri.conf.json"), "utf8"));

/** Every `[[bin]]` block's name and path. */
function declaredBinaries(): { name: string; path: string }[] {
  return [...CARGO.matchAll(/\[\[bin\]\]\s*\nname\s*=\s*"([^"]+)"\s*\npath\s*=\s*"([^"]+)"/g)].map(
    (m) => ({ name: m[1], path: m[2] }),
  );
}

/** The binary Cargo selects for a bare `cargo run`. */
function packageDefaultRun(): string | undefined {
  const packageSection = CARGO.match(/\[package\]([\s\S]*?)(?=\n\[|$)/)?.[1];
  return packageSection?.match(/^\s*default-run\s*=\s*"([^"]+)"\s*$/m)?.[1];
}

describe("the bundled binary is chosen, not inherited from cargo's ordering", () => {
  it("names the main binary in tauri.conf.json", () => {
    expect(TAURI_CONF.mainBinaryName).toBe("gitpulse");
  });

  it("selects the GUI when Tauri dev invokes cargo run without --bin", () => {
    expect(
      packageDefaultRun(),
      "[package].default-run must name the GUI or bare cargo run is ambiguous",
    ).toBe(TAURI_CONF.mainBinaryName);
  });

  it("declares that binary explicitly, and it is the GUI entry point", () => {
    const main = declaredBinaries().find((b) => b.name === TAURI_CONF.mainBinaryName);
    expect(main, `no [[bin]] named ${TAURI_CONF.mainBinaryName}`).toBeDefined();
    // src/main.rs is the Tauri entry point; the others are headless tools.
    expect(main!.path).toBe("src/main.rs");
  });

  it("declares every binary in src/bin, so discovery order decides nothing", () => {
    // The failure mode was ordering-dependent: with three auto-discovered
    // targets the bundler took the wrong one. Leaving any of them implicit
    // puts that back.
    const onDisk = readdirSync(path.join(REPO, "src-tauri", "src", "bin"))
      .filter((f) => f.endsWith(".rs"))
      .map((f) => `src/bin/${f}`)
      .sort();
    const declared = declaredBinaries()
      .map((b) => b.path)
      .filter((p) => p.startsWith("src/bin/"))
      .sort();
    expect(declared).toEqual(onDisk);
  });

  it("gives the GUI a name no other binary shadows by prefix", () => {
    // `gitpulse` and `gitpulsed` differ by one character, and a bundler that
    // globs or sorts can take the wrong one. Nothing forbids the names; this
    // records that they collide, so the explicit declarations above are load
    // bearing rather than decorative.
    const names = declaredBinaries().map((b) => b.name);
    expect(names).toContain("gitpulse");
    const shadowing = names.filter((n) => n !== "gitpulse" && n.startsWith("gitpulse"));
    expect(
      shadowing.length,
      `${shadowing.join(", ")} share the GUI's prefix — mainBinaryName and the ` +
        `explicit [[bin]] entries are what keep the bundler off them`,
    ).toBeGreaterThan(0);
  });
});
