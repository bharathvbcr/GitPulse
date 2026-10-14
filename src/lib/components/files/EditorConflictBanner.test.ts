import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import EditorConflictBanner from "./EditorConflictBanner.svelte";

const props = {
  path: "src/a.ts",
  disk: "agent wrote this\r\n",
  draft: "my draft\n",
  onReload: () => {},
  onOverwrite: () => {},
};

describe("EditorConflictBanner", () => {
  it("names the file, says the draft was kept, and offers Reload, Compare and Overwrite", () => {
    const { body } = render(EditorConflictBanner, { props });
    expect(body).toContain('role="alert"');
    expect(body).toContain("Changed on disk");
    expect(body).toContain("src/a.ts");
    expect(body).toContain("Your draft is kept and nothing was written");
    for (const label of ["Reload", "Compare", "Overwrite"]) expect(body).toContain(label);
  });

  it("holds the comparison back until Compare is pressed", () => {
    expect(render(EditorConflictBanner, { props }).body).not.toContain("editor-conflict-compare");
  });

  it("disables every action while a save is in flight", () => {
    const { body } = render(EditorConflictBanner, { props: { ...props, busy: true } });
    expect(body.match(/disabled/g)?.length).toBe(3);
  });
});
