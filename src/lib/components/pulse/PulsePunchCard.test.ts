import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("./PulsePunchCard.svelte", import.meta.url), "utf8");

describe("PulsePunchCard component", () => {
  it("delegates bucketing to the tested metric", () => {
    expect(source).toContain("computePunchCard");
    expect(source).not.toContain("86400");
  });

  it("exposes the matrix to assistive tech as a grid", () => {
    expect(source).toContain('role="gridcell"');
  });

  it("renders an empty hour slot instead of collapsing it", () => {
    expect(source).toContain("{#if count > 0}");
    expect(source).toContain("{:else}");
  });

  it("states that the after-hours reading never leaves the machine", () => {
    expect(source).toContain("never leaves your machine");
  });
});
