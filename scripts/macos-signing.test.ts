import { describe, expect, it } from "vitest";
import {
  applySigningIdentity,
  chooseSigningIdentity,
  isSigningBuild,
  parseCodesignIdentities,
} from "./macos-signing.mjs";

const DEV = "Apple Development: dev@example.com (AB12CD34EF)";
const DEVID = "Developer ID Application: Example Ltd (HAUHVD8M37)";
const HASH_A = "ABD5C633C8071199EABDF05B0D666D39BE20DEA9";
const HASH_B = "0123456789ABCDEF0123456789ABCDEF01234567";

// The shape `security find-identity -v -p codesigning` prints.
function listing(rows: Array<[string, string]>): string {
  const lines = rows.map(([hash, name], i) => `  ${i + 1}) ${hash} "${name}"`);
  return [...lines, `     ${rows.length} valid identities found`, ""].join("\n");
}

describe("parseCodesignIdentities", () => {
  it("reads every numbered identity and nothing else", () => {
    expect(parseCodesignIdentities(listing([[HASH_A, DEV], [HASH_B, DEVID]]))).toEqual([
      { hash: HASH_A, name: DEV },
      { hash: HASH_B, name: DEVID },
    ]);
    expect(parseCodesignIdentities("     0 valid identities found\n")).toEqual([]);
    expect(parseCodesignIdentities("")).toEqual([]);
  });
});

describe("chooseSigningIdentity", () => {
  it("prefers Developer ID over Apple Development and ignores other kinds", () => {
    expect(chooseSigningIdentity([{ hash: HASH_A, name: DEV }, { hash: HASH_B, name: DEVID }])).toEqual({
      value: DEVID,
      name: DEVID,
    });
    expect(chooseSigningIdentity([{ hash: HASH_A, name: DEV }])).toEqual({ value: DEV, name: DEV });
    expect(chooseSigningIdentity([{ hash: HASH_A, name: "Apple Distribution: X (AB12CD34EF)" }])).toBeNull();
    expect(chooseSigningIdentity([])).toBeNull();
  });

  it("names a shared certificate name by hash, which codesign cannot resolve by name", () => {
    expect(chooseSigningIdentity([{ hash: HASH_A, name: DEV }, { hash: HASH_B, name: DEV }])).toEqual({
      value: HASH_A,
      name: DEV,
    });
  });
});

describe("isSigningBuild", () => {
  it("is true only for build and bundle runs that sign", () => {
    expect(isSigningBuild(["build"])).toBe(true);
    expect(isSigningBuild(["build", "--bundles", "app"])).toBe(true);
    expect(isSigningBuild(["bundle"])).toBe(true);
    for (const args of [["dev"], [], ["build", "--help"], ["build", "--no-sign"], ["build", "--no-bundle"], ["info"]]) {
      expect(isSigningBuild(args)).toBe(false);
    }
  });
});

describe("applySigningIdentity", () => {
  const found = () => [{ hash: HASH_A, name: DEV }];

  it("sets Tauri's identity variable for a local macOS build", () => {
    const env: NodeJS.ProcessEnv = {};
    const note = applySigningIdentity({ args: ["build"], env, platform: "darwin", find: found });
    expect(env.APPLE_SIGNING_IDENTITY).toBe(DEV);
    expect(note).toContain(DEV);
    expect(note).toContain("APPLE_SIGNING_IDENTITY=-");
  });

  it("never overrides an explicit identity, ad-hoc included", () => {
    for (const explicit of ["-", DEVID]) {
      const env: NodeJS.ProcessEnv = { APPLE_SIGNING_IDENTITY: explicit };
      const find = () => {
        throw new Error("keychain must not be read");
      };
      expect(applySigningIdentity({ args: ["build"], env, platform: "darwin", find })).toBeNull();
      expect(env.APPLE_SIGNING_IDENTITY).toBe(explicit);
    }
  });

  it("leaves CI, other platforms and non-signing runs ad-hoc without reading the keychain", () => {
    const find = () => {
      throw new Error("keychain must not be read");
    };
    const cases: Array<{ args: string[]; env: NodeJS.ProcessEnv; platform: string }> = [
      { args: ["build"], env: { CI: "true" }, platform: "darwin" },
      { args: ["build"], env: {}, platform: "linux" },
      { args: ["dev"], env: {}, platform: "darwin" },
    ];
    for (const { args, env, platform } of cases) {
      expect(applySigningIdentity({ args, env, platform, find })).toBeNull();
      expect(env.APPLE_SIGNING_IDENTITY).toBeUndefined();
    }
  });

  it("says the build is ad-hoc when no identity exists or the keychain cannot be read", () => {
    const empty: NodeJS.ProcessEnv = {};
    expect(applySigningIdentity({ args: ["build"], env: empty, platform: "darwin", find: () => [] })).toContain(
      "signed ad-hoc",
    );
    expect(empty.APPLE_SIGNING_IDENTITY).toBeUndefined();

    const failed: NodeJS.ProcessEnv = {};
    const note = applySigningIdentity({
      args: ["build"],
      env: failed,
      platform: "darwin",
      find: () => {
        throw new Error("keychain locked");
      },
    });
    expect(note).toContain("keychain locked");
    expect(note).toContain("signed ad-hoc");
    expect(failed.APPLE_SIGNING_IDENTITY).toBeUndefined();
  });
});
