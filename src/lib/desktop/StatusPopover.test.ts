import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import StatusPopover from "./StatusPopover.svelte";
import { statusFixture } from "../../../harness/statusFixtures";

describe("compact status popover", () => {
  it("matches the reference's three-card first glance and keeps secondary controls behind Details", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("changes"), onaction: () => {} } });
    expect(body.match(/class="metric\s/g)).toHaveLength(3);
    expect(body).toContain("last fetch");
    expect(body).not.toContain('aria-label="2 stashes"');
    expect(body).not.toContain('aria-label="Go"');
    expect(body).not.toContain('aria-label="Tools"');
    expect(body).not.toContain('aria-label="Command palette"');
    expect(body).not.toContain("4 of 12 staged");
  });
  it("shows review metrics and the primary action without expanding the details", () => {
    const { body } = render(StatusPopover, { props: { snapshot: statusFixture("changes"), onaction: () => {} } });
    expect(body).toContain('aria-label="12 changed"');
    expect(body).toContain('aria-label="4 staged"');
    expect(body).toContain('aria-label="0 conflicts"');
    expect(body).toContain("Review changes");
    expect(body).toContain("ahead");
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
    expect(body).toContain("Clone repository");
    expect(body).not.toContain('aria-label="Working tree counts"');
  });
  it("surfaces urgent work in the headline while keeping shortcuts in the disclosure", () => {
    const busy = render(StatusPopover, { props: { snapshot: statusFixture("busy"), onaction: () => {} } }).body;
    expect(busy).toContain("Fetching…");
    expect(busy).not.toContain('aria-label="Workspace insights"');
    expect(busy).not.toContain("Push");
    expect(busy).not.toContain("Quick Commit");
    const parked = render(StatusPopover, { props: { snapshot: statusFixture("operation"), onaction: () => {} } }).body;
    expect(parked).toContain("Merge in progress — on main");
    expect(parked).not.toContain('aria-label="Command palette"');
  });
});
