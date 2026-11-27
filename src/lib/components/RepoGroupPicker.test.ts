import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import RepoGroupPicker from "./RepoGroupPicker.svelte";

const noop = () => {};

function picker(props: { current?: string | null; groups?: string[]; parentFolder?: string | null }) {
  return render(RepoGroupPicker, {
    props: {
      title: props.current ? "Change group" : "Add to group",
      current: props.current ?? null,
      groups: props.groups ?? [],
      parentFolder: props.parentFolder ?? null,
      onback: noop,
      onpick: noop,
      onnew: noop,
    },
  }).body;
}

const choices = (body: string) => [...body.matchAll(/data-group-choice="([^"]+)"/g)].map((m) => m[1]);

describe("RepoGroupPicker", () => {
  it("lists every open group, the parent folder and a new group, after a back row", () => {
    const body = picker({ groups: ["web", "ops", "tools", "a", "b", "c", "d"], parentFolder: "devtools" });
    // No cap: the old prompt named only five groups.
    expect(choices(body)).toEqual(["web", "ops", "tools", "a", "b", "c", "d"]);
    expect(body).toContain('data-group-parent="devtools"');
    expect(body).toContain("Parent folder (devtools)");
    expect(body).toContain("data-group-new");
    expect(body).toContain("New group…");
    expect(body.indexOf("data-group-back")).toBeLessThan(body.indexOf("data-group-choice"));
    expect(body).toContain("Add to group");
  });

  it("leaves out the tab's own group", () => {
    const body = picker({ current: "ops", groups: ["web", "ops"], parentFolder: "devtools" });
    expect(choices(body)).toEqual(["web"]);
    expect(body).toContain("Change group");
  });

  it("lists the parent folder once, on its own row, when it is already a group", () => {
    const body = picker({ groups: ["web", "devtools"], parentFolder: "devtools" });
    expect(choices(body)).toEqual(["web"]);
    expect(body).toContain('data-group-parent="devtools"');
  });

  it("hides the parent folder row when there is none or the tab is already in it", () => {
    expect(picker({ groups: ["web"], parentFolder: null })).not.toContain("data-group-parent");
    const inIt = picker({ current: "devtools", groups: ["devtools", "web"], parentFolder: "devtools" });
    expect(inIt).not.toContain("data-group-parent");
    expect(choices(inIt)).toEqual(["web"]);
  });

  it("offers only a new group when nothing else is open", () => {
    const body = picker({});
    expect(choices(body)).toEqual([]);
    expect(body).not.toContain("data-group-parent");
    expect(body).toContain("data-group-new");
    expect(body.match(/role="menuitem"/g)?.length).toBe(2);
  });
});
