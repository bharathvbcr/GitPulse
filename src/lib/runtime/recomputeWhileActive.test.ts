import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { recomputeWhileActive } from "./recomputeWhileActive";

describe("recomputeWhileActive", () => {
  it("computes on first read even when inactive", () => {
    const get = recomputeWhileActive(() => false, () => 7);
    expect(get()).toBe(7);
  });

  it("does not recompute while inactive, and recomputes the moment it is active", () => {
    let active = true;
    let source = 1;
    let computed = 0;
    const get = recomputeWhileActive(() => active, () => {
      computed += 1;
      return source;
    });
    expect(get()).toBe(1);
    active = false;
    source = 2;
    expect(get()).toBe(1);
    expect(get()).toBe(1);
    expect(computed).toBe(1);
    active = true;
    expect(get()).toBe(2);
    expect(computed).toBe(2);
  });

  it("an inactive read touches only the active flag", () => {
    const touched: string[] = [];
    const get = recomputeWhileActive(
      () => (touched.push("active"), false),
      () => (touched.push("store"), 1),
    );
    get();
    touched.length = 0;
    get();
    expect(touched).toEqual(["active"]);
  });
});

describe("App hands FleetView its real active state", () => {
  const app = readFileSync(new URL("../../App.svelte", import.meta.url), "utf8");

  it("mounts Fleet with props={{ active: fleetOpen }} through LazyView, like TaskBoard", () => {
    const mount = app.match(/<LazyView load=\{loadFleetView\}[^>]*\/>/)?.[0] ?? "";
    expect(mount).toContain("props={{ active: fleetOpen }}");
    expect(app).toMatch(/const fleetOpen = \$derived\(\$interfaceStore\.globalSurface === "fleet"\)/);
    expect(app).toContain('props={{ active: $interfaceStore.globalSurface === "tasks"');
  });
});

describe("FleetView hidden cost", () => {
  const source = readFileSync(new URL("../components/FleetView.svelte", import.meta.url), "utf8");

  it("takes the active prop and gates the store-derived grid inputs on it", () => {
    expect(source).toMatch(/active = true/);
    const gated = source.match(/recomputeWhileActive\(\s*\(\) => active/g) ?? [];
    expect(gated).toHaveLength(3);
  });
});
