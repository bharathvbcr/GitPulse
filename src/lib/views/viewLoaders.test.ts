import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { LAZY_VIEW_LOADERS, isLazyViewTab } from "./viewLoaders";
import { REGISTERED_VIEWS, VIEW_REGISTRY } from "./viewRegistry";
import type { ViewTab } from "../repos/persist";

const here = dirname(fileURLToPath(import.meta.url));
const loaderSource = readFileSync(join(here, "viewLoaders.ts"), "utf8");
const appSource = readFileSync(join(here, "../../App.svelte"), "utf8");

/** `../components/pulse/PulseView.svelte` -> `PulseView` */
function lazyComponentNames(): string[] {
  return [...loaderSource.matchAll(/import\("([^"]+\/([A-Z][A-Za-z0-9]*)\.svelte)"\)/g)].map(
    (m) => m[2],
  );
}

describe("lazy view loaders", () => {
  it("only names views that are actually registered", () => {
    const unknown = Object.keys(LAZY_VIEW_LOADERS).filter(
      (tab) => !Object.hasOwn(VIEW_REGISTRY, tab),
    );
    expect(unknown).toEqual([]);
  });

  it("code-splits exactly the inspect and more groups, keeping work eager", () => {
    // The budget guard is structural, not a list to hand-maintain: every view
    // the user reaches by an explicit tab/palette action must arrive with that
    // action. A new `inspect`/`more` view that is statically imported grows the
    // entry chunk, so it fails here rather than at the bundle-budget plugin.
    const expected = REGISTERED_VIEWS.filter((v) => v.menuGroup !== "work")
      .map((v) => v.id)
      .sort();
    expect(Object.keys(LAZY_VIEW_LOADERS).sort()).toEqual(expected);

    const eager = REGISTERED_VIEWS.filter((v) => v.menuGroup === "work").map((v) => v.id);
    expect(eager.filter((id) => isLazyViewTab(id))).toEqual([]);
  });

  it("points every loader at a component file that exists", () => {
    // Compiling all ten trees here would drag the unit suite into xterm; the
    // production build already proves they resolve. This catches the cheap
    // failure mode — a typo'd path — without the transform cost.
    const paths = [...loaderSource.matchAll(/import\("([^"]+)"\)/g)].map((m) => m[1]);
    expect(paths.length).toBe(Object.keys(LAZY_VIEW_LOADERS).length);
    const missing = paths.filter((rel) => !existsSync(join(here, rel)));
    expect(missing).toEqual([]);
  });

  it("classifies tabs with isLazyViewTab", () => {
    expect(isLazyViewTab("coverage")).toBe(true);
    expect(isLazyViewTab("work")).toBe(false);
    expect(isLazyViewTab("diff" as ViewTab)).toBe(false);
  });

  it("keeps the split views out of App.svelte's static imports", () => {
    // The regression this guards: re-adding `import CoverageViewer from …`
    // puts the view back in the entry chunk while the lazy branch still looks
    // correct, so the bundle silently regrows.
    const names = lazyComponentNames();
    expect(names.length).toBe(Object.keys(LAZY_VIEW_LOADERS).length);

    const statically = names.filter((name) =>
      new RegExp(`^\\s*import ${name} from "[^"]+\\.svelte";`, "m").test(appSource),
    );
    expect(statically).toEqual([]);
    expect(appSource).toContain('import LazyView from "./lib/components/LazyView.svelte";');
    expect(appSource).toContain("LAZY_VIEW_LOADERS");
  });
});
