/**
 * The IPC boundary for the DevCouncil payloads.
 *
 * Written after the settings harness — which answers `null` to every command
 * it does not mock — showed what an unmocked `cmd_devmap_integration_survey`
 * would do: `{#each null as plan}` throws, tears the pane's effects down, and
 * takes Settings with it. A payload that does not conform is a failed read,
 * and the panels have an error branch for exactly that.
 */

import { describe, expect, it } from "vitest";
import {
  parseInitReport,
  parseIntegrationPlan,
  parseSuiteReport,
} from "./types";

const MALFORMED = [null, undefined, "", 0, false, [], "ok"];

describe("parseInitReport", () => {
  const good = {
    repo: "/a",
    state_dir: "/a/.devmap",
    exclude: { status: "added", file: "/a/.git/info/exclude", pattern: "/.devmap/" },
    workspace_registry: "/a/.devmap/workspace.json",
    workspace_reason: null,
    devmap_available: true,
  };

  it("accepts a well-formed report", () => {
    expect(parseInitReport(good)).toEqual(good);
  });

  it("rejects every malformed payload rather than inventing a result", () => {
    for (const payload of MALFORMED) {
      expect(() => parseInitReport(payload), String(payload)).toThrow();
    }
  });

  it("rejects a report with no exclude outcome", () => {
    expect(() => parseInitReport({ ...good, exclude: null })).toThrow(/exclude/);
    expect(() => parseInitReport({ ...good, exclude: {} })).toThrow(/exclude/);
  });

  it("never reads a missing devmap_available as installed", () => {
    const parsed = parseInitReport({ ...good, devmap_available: undefined });
    expect(parsed.devmap_available).toBe(false);
  });

  it("normalizes an absent registry to null rather than undefined", () => {
    const parsed = parseInitReport({
      ...good,
      workspace_registry: undefined,
      workspace_reason: undefined,
    });
    expect(parsed.workspace_registry).toBeNull();
    expect(parsed.workspace_reason).toBeNull();
  });
});

describe("parseSuiteReport", () => {
  const good = {
    suite: { components: [], presets: [], complete: true },
    doctor: { available: true },
    doctor_reason: null,
    warnings: ["skew"],
  };

  it("accepts a well-formed report", () => {
    const parsed = parseSuiteReport(good);
    expect(parsed.warnings).toEqual(["skew"]);
    expect(parsed.suite.complete).toBe(true);
  });

  it("rejects every malformed payload", () => {
    for (const payload of MALFORMED) {
      expect(() => parseSuiteReport(payload), String(payload)).toThrow();
    }
    expect(() => parseSuiteReport({ suite: { components: "no", presets: [] } })).toThrow();
  });

  it("turns a missing warning list into 'not checked', never into 'clean'", () => {
    // The failure this exists to prevent: a probe whose warnings never arrived
    // rendering identically to one that ran and found none.
    const parsed = parseSuiteReport({ ...good, warnings: undefined, doctor_reason: null });
    expect(parsed.warnings).toEqual([]);
    expect(parsed.doctor_reason).toMatch(/no warning list/);
  });

  it("keeps a stated reason rather than replacing it", () => {
    const parsed = parseSuiteReport({
      ...good,
      warnings: undefined,
      doctor_reason: "no repository is open",
    });
    expect(parsed.doctor_reason).toBe("no repository is open");
  });

  it("drops non-string warnings instead of rendering them", () => {
    const parsed = parseSuiteReport({ ...good, warnings: ["real", 7, null, { a: 1 }] });
    expect(parsed.warnings).toEqual(["real"]);
  });

  it("never reads a missing complete flag as complete", () => {
    const parsed = parseSuiteReport({ ...good, suite: { components: [], presets: [] } });
    expect(parsed.suite.complete).toBe(false);
  });
});

describe("parseIntegrationPlan", () => {
  const good = {
    available: true,
    host: "claude",
    repo: "/a",
    applied: false,
    reason: null,
    entries: [],
    notes: ["a note"],
    repo_changes: 2,
    outside_changes: 1,
    protected: ["/a/AGENTS.md"],
  };

  it("accepts a well-formed plan", () => {
    expect(parseIntegrationPlan(good)).toEqual(good);
  });

  it("rejects every malformed payload", () => {
    for (const payload of MALFORMED) {
      expect(() => parseIntegrationPlan(payload), String(payload)).toThrow();
    }
    expect(() => parseIntegrationPlan({ available: true })).toThrow(/host/);
  });

  it("refuses an available plan with no entry list", () => {
    // Defaulting to `[]` would present a pending write as nothing to do.
    expect(() => parseIntegrationPlan({ ...good, entries: undefined })).toThrow(/entry list/);
  });

  it("allows an unavailable plan to carry no entries", () => {
    const parsed = parseIntegrationPlan({
      available: false,
      host: "cursor",
      reason: "devmap is not installed",
    });
    expect(parsed.available).toBe(false);
    expect(parsed.entries).toEqual([]);
    expect(parsed.reason).toBe("devmap is not installed");
  });

  it("never reads a missing availability flag as available", () => {
    expect(parseIntegrationPlan({ host: "codex" }).available).toBe(false);
  });

  it("floors nonsensical change counts at zero rather than rendering them", () => {
    const parsed = parseIntegrationPlan({
      ...good,
      repo_changes: -3,
      outside_changes: Number.NaN,
    });
    expect(parsed.repo_changes).toBe(0);
    expect(parsed.outside_changes).toBe(0);
  });

  it("drops non-string notes and protected paths", () => {
    const parsed = parseIntegrationPlan({ ...good, notes: [1, "keep"], protected: [null, "/p"] });
    expect(parsed.notes).toEqual(["keep"]);
    expect(parsed.protected).toEqual(["/p"]);
  });
});
