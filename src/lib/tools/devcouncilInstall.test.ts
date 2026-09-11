import { describe, expect, it } from "vitest";
import {
  PUBLIC_DEVCOUNCIL_GIT,
  buildDevcouncilInstallCommand,
} from "./devcouncilInstall";

describe("buildDevcouncilInstallCommand", () => {
  it("installs standalone devmap from a checkout without the Go host", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "devmap",
      sourceCheckout: "/Users/me/DevCouncil",
    });
    expect(got.command).toContain("install-components.sh");
    expect(got.command).toContain("devmap");
    expect(got.command).not.toContain("install.sh'");
    expect(got.command).not.toContain("uv");
    expect(got.runnable).toBe(true);
  });

  it("uses the unified script for full DevCouncil from a checkout", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "all",
      sourceCheckout: "/opt/DevCouncil",
    });
    expect(got.command).toContain("scripts/install.sh");
    expect(got.command).not.toContain("uv");
  });

  it("installs the analysis suite from a checkout without the host script", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "analysis",
      sourceCheckout: "/opt/DevCouncil",
    });
    expect(got.command).toContain("install-components.sh");
    expect(got.command).toContain("analysis");
  });

  it("falls back to cargo --git when there is no checkout", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "devmap",
      sourceCheckout: null,
    });
    expect(got.command).toBe(
      `cargo install --git ${PUBLIC_DEVCOUNCIL_GIT} --locked --force devmap-cli`,
    );
  });

  it("wraps a remote bulk install so Console can run it as one argv", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "analysis",
      sourceCheckout: null,
    });
    expect(got.command.startsWith("bash -c ")).toBe(true);
    expect(got.command).toContain("dc-store");
    expect(got.command).not.toContain("uv");
  });

  it("quotes a checkout path that contains a single quote", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "devmap",
      sourceCheckout: "/Users/o'brien/DevCouncil",
    });
    expect(got.runnable).toBe(true);
    expect(got.command).toContain("/Users/o'\\''brien/DevCouncil");
  });

  it("refuses a checkout path that contains a newline", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "devmap",
      sourceCheckout: "/tmp/DevCouncil\n/bin/true",
    });
    expect(got.runnable).toBe(false);
    expect(got.command).toBe("");
  });

  it("refuses a checkout path that contains a NUL", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "devmap",
      sourceCheckout: "/tmp/DevCouncil\0x",
    });
    expect(got.runnable).toBe(false);
    expect(got.command).toBe("");
  });

  it("single-quotes Windows paths so $HOME is not expanded", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "devmap",
      sourceCheckout: "C:\\Users\\$HOME\\DevCouncil",
      windows: true,
    });
    expect(got.command).toMatch(/-File '.*\$HOME.*'/);
    expect(got.command).not.toMatch(/-File "/);
  });

  it("emits a PowerShell file invocation on Windows checkouts", () => {
    const got = buildDevcouncilInstallCommand({
      preset: "devmap",
      sourceCheckout: "C:\\src\\DevCouncil",
      windows: true,
    });
    expect(got.command).toContain("powershell");
    expect(got.command).toContain("install.ps1");
    expect(got.command).toContain("-Components devmap");
  });
});
