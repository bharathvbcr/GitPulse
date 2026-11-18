import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import SettingSegment from "./SettingSegment.svelte";

const options = [
  { value: "names", label: "Names" },
  { value: "contents", label: "Contents" },
] as const;

describe("SettingSegment size", () => {
  it("leaves the group unmarked at the default size", () => {
    const { body } = render(SettingSegment, { props: { ariaLabel: "Search in", options, value: "names" } });
    expect(body).toContain('class="gp-segmented"');
    expect(body).not.toContain("data-size");
  });

  it("marks the group data-size=sm when asked for the compact size", () => {
    const { body } = render(SettingSegment, {
      props: { ariaLabel: "Search in", options, value: "names", size: "sm" },
    });
    expect(body).toMatch(/class="gp-segmented"[^>]*data-size="sm"/);
  });

  it("styles a compact group's buttons in metadata type, keeping the 24px target", () => {
    const css = readFileSync(new URL("../../app.css", import.meta.url), "utf8");
    const segButton = css.slice(css.indexOf("@utility gp-seg-btn {"), css.indexOf("@utility gp-field {"));
    expect(segButton).toContain("min-height: 24px;");
    expect(segButton).toMatch(/\[data-size='sm'\] > & \{\s*@apply px-2 text-meta;/);
  });

  // Explorer's search field used this size for its Names | Contents segment
  // until that became a single contents toggle; Reflog's source switch is the
  // compact size's caller now.
  it("is the compact size in Reflog's source switch", () => {
    const reflog = readFileSync(new URL("./ReflogViewer.svelte", import.meta.url), "utf8");
    const segment = reflog.slice(reflog.indexOf("<SettingSegment"), reflog.indexOf("/>", reflog.indexOf("<SettingSegment")));
    expect(segment).toContain('ariaLabel="Reflog source"');
    expect(segment).toContain('size="sm"');
  });
});
