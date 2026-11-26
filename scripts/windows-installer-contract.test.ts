import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const TAURI_CONF_PATH = path.join(REPO_ROOT, "src-tauri", "tauri.conf.json");

/**
 * Compute RFC 4122 UUIDv5 (DNS namespace) matching tauri-bundler's WiX
 * UpgradeCode derivation (`Uuid::new_v5(&Uuid::NAMESPACE_DNS, format!("{product_name}.exe.app.x64").as_bytes())`).
 */
function tauriWixUpgradeCodeV5(productName: string): string {
  // RFC 4122 DNS namespace UUID: 6ba7b810-9dad-11d1-80b4-00c04fd430c8
  const dnsNs = Buffer.from("6ba7b8109dad11d180b400c04fd430c8", "hex");
  const name = Buffer.from(`${productName}.exe.app.x64`, "utf8");
  const digest = createHash("sha1").update(dnsNs).update(name).digest();
  const bytes = Buffer.from(digest.subarray(0, 16));
  bytes[6] = (bytes[6] & 0x0f) | 0x50; // version 5
  bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant RFC 4122
  const hex = bytes.toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20, 32)}`;
}

describe("Windows installer upgrade continuity", () => {
  const tauriConf = JSON.parse(readFileSync(TAURI_CONF_PATH, "utf8")) as {
    productName: string;
    mainBinaryName: string;
    identifier: string;
    bundle: {
      longDescription?: string;
      resources?: Record<string, string>;
      windows?: {
        wix?: { version?: string; upgradeCode?: string };
        nsis?: { installerHooks?: string };
      };
    };
  };

  it("pins bundle.windows.wix.upgradeCode to legacy GitPulse UUIDv5 GUID", () => {
    const legacyGitPulseUpgradeCode = tauriWixUpgradeCodeV5("GitPulse");
    expect(legacyGitPulseUpgradeCode).toBe("b6ba8858-9476-58c2-a749-8f985b9a9d0a");
    expect(tauriConf.bundle.windows?.wix?.upgradeCode).toBe(legacyGitPulseUpgradeCode);
  });

  it("configures Waylode product identity and NSIS pre-install cleanup hook", () => {
    expect(tauriConf.productName).toBe("Waylode");
    expect(tauriConf.mainBinaryName).toBe("waylode");
    expect(tauriConf.identifier).toBe("dev.vbcr.waylode");
    expect(tauriConf.bundle.longDescription).toMatch(/^Waylode is\b/);
    expect(tauriConf.bundle.resources).toEqual({ "../plugins/waylode/": "plugin/" });

    const hookRel = tauriConf.bundle.windows?.nsis?.installerHooks;
    expect(hookRel).toBe("windows/nsis-hooks.nsh");
    const hookPath = path.join(REPO_ROOT, "src-tauri", hookRel!);
    expect(existsSync(hookPath)).toBe(true);
    const hookText = readFileSync(hookPath, "utf8");
    expect(hookText).toContain("!macro NSIS_HOOK_PREINSTALL");
    expect(hookText).toContain("Uninstall\\GitPulse");
  });
});
