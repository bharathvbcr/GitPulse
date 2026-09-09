import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import StatusPopover from "./StatusPopover.svelte";
import { statusFixture } from "../../../harness/statusFixtures";

describe("compact status popover", () => {
  it("shows review metrics and the primary action without expanding the details", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("changes"), onaction: () => {} } });
    expect(body).toContain('aria-label="12 changed"');
    expect(body).toContain('aria-label="4 staged"');
    expect(body).toContain('aria-label="0 conflicts"');
    expect(body).toContain("Review changes");
    expect(body).not.toContain('id="status-details"');
    expect(body).not.toContain("listed stashes");
  });
  it("keeps missing values distinct from a clean working tree", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("unavailable"), onaction: () => {} } });
    expect(body).toContain('aria-label="Unknown changed"');
    expect(body).toContain("Status unavailable");
    expect(body).toContain("Try again");
    expect(body).not.toContain("All changes committed");
  });
  it("offers a short first-open state instead of empty metric cards", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("empty"), onaction: () => {} } });
    expect(body).toContain("Open repository");
    expect(body).not.toContain('aria-label="Working tree counts"');
  });
});
