import { linkSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, symlinkSync, truncateSync, writeFileSync } from "node:fs";
import { execFile } from "node:child_process";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { gzipSync } from "node:zlib";
import { afterAll, describe, expect, it } from "vitest";
import {
  PLATFORM_INSTALLERS,
  bundleSpecs,
  checkSevenZipListing,
  declaredBinaryNames,
  expectedAssetNames,
  expectedInstallerNames,
  inspectBundleContents,
  inspectBundles,
  inspectReleaseAssets,
  supplementNames,
} from "./check-release-assets.mjs";

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(new URL("./check-release-assets.mjs", import.meta.url));
const tempDirs: string[] = [];

afterAll(async () => {
  while (tempDirs.length > 0) {
    const dir = tempDirs.pop();
    if (dir) await rm(dir, { recursive: true, force: true });
  }
});

function payload(version = "0.0.3", names = expectedAssetNames(version), size = 10) {
  return JSON.stringify({
    assets: names.map((name) => ({ name, size, state: "uploaded" })),
  });
}

async function runScript(tag: string, source: string) {
  const dir = await mkdtemp(path.join(tmpdir(), "gitpulse-release-assets-"));
  tempDirs.push(dir);
  const jsonPath = path.join(dir, "assets.json");
  await writeFile(jsonPath, source);
  try {
    const { stdout, stderr } = await execFileAsync(process.execPath, [
      scriptPath,
      "--tag",
      tag,
      "--json",
      jsonPath,
    ]);
    return { code: 0, output: stdout + stderr };
  } catch (err) {
    const failure = err as { code?: number | null; stdout?: string; stderr?: string };
    return {
      code: failure.code ?? 1,
      output: (failure.stdout ?? "") + (failure.stderr ?? ""),
    };
  }
}

describe("release asset completeness contract", () => {
  it("accepts the exact non-empty asset manifest for the configured matrix", async () => {
    const result = inspectReleaseAssets({ tag: "v0.0.3", json: payload() });
    expect(result.ok).toBe(true);
    expect(result.violations).toEqual([]);

    const cli = await runScript("v0.0.3", payload());
    expect(cli.code).toBe(0);
    expect(cli.output).toContain("OK: release asset manifest holds");
  });

  it("rejects stale, wrong-version, missing, and unexpected assets", async () => {
    const names = expectedAssetNames("0.0.3").filter((name) => !name.endsWith(".dmg"));
    names.push("GitPulse_0.0.2_universal.dmg", "old-release.deb");
    const result = inspectReleaseAssets({
      tag: "v0.0.3",
      json: payload("0.0.3", names),
    });
    expect(result.ok).toBe(false);
    expect(result.violations.join("\n")).toMatch(/missing: GitPulse_0\.0\.3_universal\.dmg/);
    expect(result.violations.join("\n")).toMatch(/unexpected: GitPulse_0\.0\.2_universal\.dmg/);
    expect(result.violations.join("\n")).toMatch(/unexpected: old-release\.deb/);

    const cli = await runScript("v0.0.3", payload("0.0.3", names));
    expect(cli.code).toBe(1);
  });

  it("treats duplicate names, zero-size assets, malformed JSON, and bad tags as invalid input", async () => {
    const names = expectedAssetNames("0.0.3");
    const duplicate = JSON.stringify({
      assets: [
        ...names.map((name) => ({ name, size: 10, state: "uploaded" })),
        { name: names[0], size: 10, state: "uploaded" },
      ],
    });
    expect(inspectReleaseAssets({ tag: "v0.0.3", json: duplicate }).invalid).toBe(true);
    expect(
      inspectReleaseAssets({
        tag: "v0.0.3",
        json: payload("0.0.3", names, 0),
      }).invalid,
    ).toBe(true);

    expect(inspectReleaseAssets({ tag: "v0.0.3", json: "not json" }).invalid).toBe(true);
    const cli = await runScript("release-0.0.3", payload());
    expect(cli.code).toBe(2);
  });

  it("accepts the real payload gh produced for v0.0.2", () => {
    // scripts/fixtures/gh-release-view-v1.json is `gh release view --json
    // assets` for a real release — the exact command release.yml runs — with
    // per-account noise (urls, ids, download counts) dropped and every other
    // field kept verbatim.
    //
    // This checker runs only on a `v*` tag, so a wrong expectation here would
    // surface at release time after the whole matrix had built. That is exactly
    // what happened when `tauri-apps/tauri-action` went v0 -> v1 and started
    // versioning the macOS updater archive.
    const payload = readFileSync(
      new URL("./fixtures/gh-release-view-v1.json", import.meta.url),
      "utf8",
    );
    //
    // That release predates the ARM legs and the attest stage, so its seven
    // names must all still be expected, and what it lacks must be exactly the
    // five ARM installers and three supplements added since — no renamed
    // x64 or universal asset may hide in either list.
    const result = inspectReleaseAssets({ tag: "v0.0.3", json: payload });
    expect(result.invalid).toBe(false);
    expect(result.violations.filter((violation) => !violation.startsWith("missing: "))).toEqual([]);
    const supplements = Object.values(supplementNames("0.0.3"));
    const arm = expectedInstallerNames("0.0.3").filter((name) => /aarch64|arm64/.test(name));
    expect(arm).toHaveLength(5);
    expect(result.violations.map((violation) => violation.slice("missing: ".length)).sort())
      .toEqual([...arm, ...supplements].sort());
    expect(result.actual).toEqual(expectedInstallerNames("0.0.3").filter((name) => !arm.includes(name)));
  });

  it("rejects the pre-v1 archive name, so the rename cannot silently come back", () => {
    // The v0.0.2 payload is kept verbatim: under `tauri-action@v0` the macOS
    // updater archive carried no version. It is a real payload that this
    // manifest must now refuse, which is what proves the manifest tracks the
    // action's naming rather than accepting whatever shows up.
    const payload = readFileSync(
      new URL("./fixtures/gh-release-view.json", import.meta.url),
      "utf8",
    );
    const result = inspectReleaseAssets({ tag: "v0.0.2", json: payload });
    expect(result.ok).toBe(false);
    expect(result.violations).toContain("missing: GitPulse_0.0.2_universal.app.tar.gz");
    expect(result.violations).toContain("unexpected: GitPulse_universal.app.tar.gz");
  });

  it("notices if a platform silently stops producing an installer", () => {
    // The failure this exists to catch: a green matrix that uploaded less
    // than it should have.
    const payload = JSON.parse(
      readFileSync(new URL("./fixtures/gh-release-view.json", import.meta.url), "utf8"),
    ) as { assets: Array<{ name: string }> };
    payload.assets = payload.assets.filter((asset) => !asset.name.endsWith(".msi"));
    const result = inspectReleaseAssets({ tag: "v0.0.2", json: JSON.stringify(payload) });
    expect(result.ok).toBe(false);
    expect(result.violations.join(" ")).toContain(".msi");
  });
});


describe("release metadata cannot imply an unperformed upload", () => {
  it.each([undefined, null, "new", "starter", "failed"])("rejects upload state %s", (state) => {
    const assets = expectedAssetNames("1.2.3").map(name => ({ name, size: 10, state }));
    expect(inspectReleaseAssets({tag: "v1.2.3", json: {assets}}).invalid).toBe(true);
  });
});

describe("installer manifest follows the release matrix", () => {
  it("names exactly the runners release.yml builds on", () => {
    const workflow = readFileSync(new URL("../.github/workflows/release.yml", import.meta.url), "utf8");
    const matrix = workflow.slice(workflow.indexOf("\n  release:"), workflow.indexOf("\n  attest:"));
    const platforms = [...matrix.matchAll(/- platform: '([^']+)'/g)].map((match) => match[1]).sort();
    expect(Object.keys(PLATFORM_INSTALLERS).sort()).toEqual(platforms);
  });

  it("names exactly the ARM runners ci.yml's arm-bundle job proves", () => {
    const ci = readFileSync(new URL("../.github/workflows/ci.yml", import.meta.url), "utf8");
    const job = ci.slice(ci.indexOf("\n  arm-bundle:"), ci.indexOf("\n  gusset:"));
    const legs = /platform: \[([^\]]+)\]/.exec(job)?.[1].split(",").map((leg) => leg.trim()).sort();
    const arm = Object.keys(PLATFORM_INSTALLERS).filter((platform) => platform.endsWith("-arm")).sort();
    expect(arm).toHaveLength(2);
    expect(legs).toEqual(arm);
    expect(job).toContain("node scripts/check-release-assets.mjs --bundles");
  });
});

describe("local bundle names", () => {
  const linuxArm = [
    "/w/src-tauri/target/release/bundle/deb/GitPulse_1.5.0_arm64.deb",
    "/w/src-tauri/target/release/bundle/rpm/GitPulse-1.5.0-1.aarch64.rpm",
    "/w/src-tauri/target/release/bundle/appimage/GitPulse_1.5.0_aarch64.AppImage",
  ];
  const windowsArm = [
    "D:\\a\\GitPulse\\src-tauri\\target\\release\\bundle\\msi\\GitPulse_1.5.0_arm64_en-US.msi",
    "D:\\a\\GitPulse\\src-tauri\\target\\release\\bundle\\nsis\\GitPulse_1.5.0_arm64-setup.exe",
  ];

  it("accepts each ARM runner's bundler output, Windows separators included", () => {
    expect(inspectBundles({ platform: "ubuntu-22.04-arm", version: "1.5.0", paths: linuxArm }).ok).toBe(true);
    expect(inspectBundles({ platform: "windows-11-arm", version: "1.5.0", paths: windowsArm }).ok).toBe(true);
  });

  it("rejects an x64 installer produced on an ARM runner, and a missing format", () => {
    const result = inspectBundles({
      platform: "ubuntu-22.04-arm",
      version: "1.5.0",
      paths: [linuxArm[0], linuxArm[1], "/w/bundle/appimage/GitPulse_1.5.0_amd64.AppImage"],
    });
    expect(result.ok).toBe(false);
    expect(result.violations).toEqual([
      "missing: GitPulse_1.5.0_aarch64.AppImage",
      "unexpected: GitPulse_1.5.0_amd64.AppImage",
    ]);
  });

  it("refuses what it cannot judge rather than passing it", () => {
    expect(() => inspectBundles({ platform: "macos-latest", version: "1.5.0", paths: ["/x/GitPulse.app.tar.gz"] })).toThrow(/rewritten at upload/);
    expect(() => inspectBundles({ platform: "solaris", version: "1.5.0", paths: linuxArm })).toThrow(/unknown release platform/);
    expect(() => inspectBundles({ platform: "windows-11-arm", version: "1.5.0", paths: [] })).toThrow(/non-empty array/);
  });

  it("checks against tauri.conf.json's version from the CLI", async () => {
    const config = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8")) as { version: string };
    const paths = PLATFORM_INSTALLERS["windows-11-arm"](config.version).map((name) => `C:\\b\\${name}`);
    const ok = await execFileAsync(process.execPath, [scriptPath, "--bundles", JSON.stringify(paths), "--platform", "windows-11-arm"]);
    expect(ok.stdout).toContain("OK: windows-11-arm bundled exactly");
    await expect(execFileAsync(process.execPath, [scriptPath, "--bundles", JSON.stringify(paths.slice(1)), "--platform", "windows-11-arm"]))
      .rejects.toMatchObject({ code: 1 });
    await expect(execFileAsync(process.execPath, [scriptPath, "--bundles", "[]", "--platform", "macos-latest", "--tag", "v1.0.0"]))
      .rejects.toMatchObject({ code: 2 });
  });
});

describe("declared binaries", () => {
  it("reads every [[bin]] name wherever its key sits, and ignores comments and other tables", () => {
    const toml = [
      "[package]",
      'name = "not-a-bin"',
      "[[bin]]",
      'path = "src/main.rs"   # the key order is the author\'s',
      'name = "alpha"',
      "# [[bin]]",
      '# name = "commented-out"',
      "[[bin]]  # trailing comment",
      'name = "beta-two" # and here',
      "required-features = []",
      "[[test]]",
      'name = "a-test-not-a-bin"',
      "[[bin]]",
      'name="gamma"',
      "[dependencies]",
      'name = "dep"',
    ].join("\r\n");
    expect(declaredBinaryNames(toml)).toEqual(["alpha", "beta-two", "gamma"]);
  });

  it("reads TOML literal strings, quoted keys and a spaced header", () => {
    expect(declaredBinaryNames("[[bin]]\nname = 'alpha'\n[[ bin ]]\n\"name\" = \"beta\"\n[[\"bin\"]]\nname = 'gamma' # c\n")).toEqual(["alpha", "beta", "gamma"]);
  });

  it.each([
    ["a [[bin]] without a name", "[[bin]]\npath = \"a.rs\"\n[[bin]]\nname = \"b\"\n"],
    ["a name that is not a plain string", "[[bin]]\nname = 5\n"],
    ["two names in one table", "[[bin]]\nname = \"a\"\nname = \"b\"\n"],
    ["an inline bin array", 'bin = [{ name = "alpha", path = "a.rs" }]\n'],
    ["a dotted bin table", '[bin.alpha]\nname = "alpha"\n'],
    ["a manifest with no [[bin]] at all", '[package]\nname = "x"\n'],
    ["an empty manifest", ""],
  ])("refuses %s rather than returning a shorter list", (_label, toml) => {
    expect(() => declaredBinaryNames(toml)).toThrow(/Cargo\.toml/);
  });

  it("does not mistake a dependency named bin for a binary table", () => {
    expect(declaredBinaryNames('[dependencies]\nbin = "1"\n[[bin]]\nname = "alpha"\n')).toEqual(["alpha"]);
  });

  it("finds the five executables this crate builds, so a sixth would be required automatically", () => {
    const cargo = readFileSync(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8");
    expect(declaredBinaryNames(cargo).sort()).toEqual(["gitpulse", "gitpulse-hook", "gitpulse-mcp", "gitpulse-ptyd", "gitpulsed"]);
  });
});

/** Builders for the installer layouts the bundle check opens. */
const VERSION = "1.2.3";
const BINS = ["gitpulse", "gitpulse-hook", "gitpulse-mcp", "gitpulse-ptyd", "gitpulsed"];
const ARM64 = 0x0100000c;
const X86_64 = 0x01000007;

/** A fat header whose slices each hold 8 bytes, laid out after the header. */
function fat(cpus: number[]): Buffer {
  const headerEnd = 8 + cpus.length * 20;
  const head = Buffer.alloc(headerEnd + cpus.length * 8);
  head.writeUInt32BE(0xcafebabe, 0);
  head.writeUInt32BE(cpus.length, 4);
  cpus.forEach((cpu, index) => {
    head.writeUInt32BE(cpu, 8 + index * 20);
    head.writeUInt32BE(headerEnd + index * 8, 8 + index * 20 + 8);
    head.writeUInt32BE(8, 8 + index * 20 + 12);
  });
  return head;
}
const universal = () => fat([ARM64, X86_64]);
function elf(machine: number): Buffer {
  const out = Buffer.alloc(64);
  out.writeUInt32BE(0x7f454c46, 0);
  out[4] = 2;
  out[5] = 1;
  out.writeUInt16LE(machine, 18);
  return out;
}
function pe(machine: number): Buffer {
  const out = Buffer.alloc(0x100);
  out.write("MZ", 0, "latin1");
  out.writeUInt32LE(0x80, 0x3c);
  out.write("PE\0\0", 0x80, "latin1");
  out.writeUInt16LE(machine, 0x84);
  return out;
}

function octal(value: number, width: number): string {
  return `${value.toString(8).padStart(width - 1, "0")}\0`;
}
function tarHeader(name: string, size: number, type = "0", mode = 0o755): Buffer {
  const header = Buffer.alloc(512);
  header.write(name, 0, 100, "utf8");
  header.write(octal(mode, 8), 100, "latin1");
  header.write(octal(0, 8), 108, "latin1");
  header.write(octal(0, 8), 116, "latin1");
  header.write(octal(size, 12), 124, "latin1");
  header.write(octal(0, 12), 136, "latin1");
  header.write("        ", 148, "latin1");
  header.write(type, 156, "latin1");
  header.write("ustar\0" + "00", 257, "latin1");
  let sum = 0;
  for (const byte of header) sum += byte;
  header.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148, "latin1");
  return header;
}
const padded = (data: Buffer) => Buffer.concat([data, Buffer.alloc((512 - (data.length % 512)) % 512)]);
function tarEntry(name: string, data: Buffer, opts: { type?: string; mode?: number; declaredSize?: number } = {}): Buffer {
  const size = opts.declaredSize ?? data.length;
  return Buffer.concat([tarHeader(name, size, opts.type ?? "0", opts.mode ?? 0o755), padded(data)]);
}
function paxRecord(key: string, value: string): string {
  const body = ` ${key}=${value}\n`;
  let length = body.length + 1;
  while (String(length).length + body.length !== length) length = String(length).length + body.length;
  return `${length}${body}`;
}
function paxEntry(records: Record<string, string>, realName: string, data: Buffer, mode = 0o755): Buffer {
  const text = Buffer.from(Object.entries(records).map(([key, value]) => paxRecord(key, value)).join(""));
  return Buffer.concat([tarEntry("PaxHeader/x", text, { type: "x", mode: 0o644 }), tarEntry(realName, data, { mode })]);
}
const tarball = (...entries: Buffer[]) => Buffer.concat([...entries, Buffer.alloc(1024)]);

function arMember(name: string, data: Buffer): Buffer {
  const header = Buffer.from(`${name.padEnd(16)}${"0".padEnd(12)}${"0".padEnd(6)}${"0".padEnd(6)}${"100644".padEnd(8)}${String(data.length).padEnd(10)}\`\n`, "latin1");
  return Buffer.concat([header, data, data.length % 2 ? Buffer.from("\n") : Buffer.alloc(0)]);
}
const debOf = (dataName: string, data: Buffer) =>
  Buffer.concat([Buffer.from("!<arch>\n"), arMember("debian-binary", Buffer.from("2.0\n")), arMember("control.tar.gz", gzipSync(tarball())), arMember(dataName, data)]);

type Extracted = Record<string, Buffer | { symlink: string } | { hardlink: string }>;

/** A scratch dir holding all seven bundled installers, with per-asset overrides. */
async function bundles(overrides: Record<string, Buffer | Extracted> = {}, bins = BINS) {
  const dir = await mkdtemp(path.join(tmpdir(), "gitpulse-bundles-"));
  tempDirs.push(dir);
  const files: Record<string, string> = {};
  const windows = new Map<string, Extracted>();
  for (const spec of bundleSpecs(VERSION)) {
    const target = path.join(dir, spec.asset);
    files[spec.asset] = target;
    const override = overrides[spec.asset];
    if (spec.format === "app-tar") {
      const body = override instanceof Buffer ? override
        : gzipSync(tarball(...bins.map((bin) => tarEntry(`./GitPulse.app/Contents/MacOS/${bin}`, universal()))));
      writeFileSync(target, body);
    } else if (spec.format === "deb") {
      const machine = spec.arch === "arm64" ? 0xb7 : 0x3e;
      const body = override instanceof Buffer ? override
        : debOf("data.tar.gz", gzipSync(tarball(...bins.map((bin) => tarEntry(`./usr/bin/${bin}`, elf(machine))))));
      writeFileSync(target, body);
    } else {
      writeFileSync(target, "installer");
      const machine = spec.arch === "arm64" ? 0xaa64 : 0x8664;
      const id = (bin: string) => (spec.format === "msi" ? (bin === "gitpulse" ? "Path" : `Bin_${bin.replaceAll("-", "_")}`) : `${bin}.exe`);
      const layout = override === undefined || Buffer.isBuffer(override) ? undefined : override;
      windows.set(target, layout ?? Object.fromEntries(bins.map((bin) => [id(bin), pe(machine)])));
    }
  }
  const extract = (archive: string, outDir: string) => {
    const layout = windows.get(archive);
    if (!layout) throw new Error(`unexpected archive ${archive}`);
    for (const [name, content] of Object.entries(layout)) {
      const dest = path.join(outDir, name);
      mkdirSync(path.dirname(dest), { recursive: true });
      if (Buffer.isBuffer(content)) writeFileSync(dest, content);
      else if ("symlink" in content) symlinkSync(content.symlink, dest);
      else linkSync(path.join(outDir, content.hardlink), dest);
    }
  };
  return { files, extract, dir };
}
const check = (made: Awaited<ReturnType<typeof bundles>>, binaries = BINS, extra: Record<string, unknown> = {}) =>
  inspectBundleContents({ version: VERSION, binaries, files: made.files, extract: made.extract, ...extra });
const MAC = `GitPulse_${VERSION}_universal.app.tar.gz`;
const DEB = `GitPulse_${VERSION}_amd64.deb`;
const DEB_ARM = `GitPulse_${VERSION}_arm64.deb`;
const MSI = `GitPulse_${VERSION}_x64_en-US.msi`;
const MSI_ARM = `GitPulse_${VERSION}_arm64_en-US.msi`;
const NSIS = `GitPulse_${VERSION}_x64-setup.exe`;
const NSIS_ARM = `GitPulse_${VERSION}_arm64-setup.exe`;
const macTar = (...entries: Buffer[]) => gzipSync(tarball(...entries));
const macBin = (bin: string, data = universal(), opts = {}) => tarEntry(`GitPulse.app/Contents/MacOS/${bin}`, data, opts);
const allMacBins = (skip = "") => BINS.filter((bin) => bin !== skip).map((bin) => macBin(bin));

describe("7z listing", () => {
  const listing = (...entries: string[]) => `\nListing archive: x\n\n--\nPath = /a/x.msi\nType = Compound\n----------\n${entries.join("\n\n")}\n`;
  it("accepts entries inside the output directory within the size bound", () => {
    expect(() => checkSevenZipListing(listing("Path = gitpulse.exe\nSize = 10", "Path = $PLUGINSDIR\\System.dll\nSize = 5", "Path = dir\nFolder = +", "Path = [NSIS].nsi\nSize = \nPacked Size = "), 100)).not.toThrow();
  });
  it.each([
    ["an absolute path", "Path = /etc/passwd\nSize = 1", /leaves the output directory/],
    ["a drive-letter path", "Path = C:\\Windows\\x.dll\nSize = 1", /leaves the output directory/],
    ["a parent segment", "Path = a\\..\\..\\x\nSize = 1", /leaves the output directory/],
    ["a size that is not a number", "Path = a\nSize = lots", /not a number/],
    ["declared sizes over the bound", "Path = a\nSize = 101", /more than 100 bytes/],
  ])("refuses %s before anything is extracted", (_label, entry, expected) => {
    expect(() => checkSevenZipListing(listing(entry), 100)).toThrow(expected);
  });
  it("refuses a listing with no entries", () => {
    expect(() => checkSevenZipListing("Listing archive: x\n", 100)).toThrow(/no entries/);
    expect(() => checkSevenZipListing(listing(), 100)).toThrow(/no entries/);
  });
  it("refuses an extraction that left a link pointing outside its directory", async () => {
    const outside = await bundles();
    const link = inspectBundleContents({
      version: VERSION, binaries: BINS, files: outside.files,
      extract: (archive, outDir) => { outside.extract(archive, outDir); symlinkSync("/etc", path.join(outDir, "escape")); },
    });
    expect(link.invalid).toBe(true);
    expect(link.violations.some((violation) => violation.includes("outside its directory"))).toBe(true);
  });
});

describe("bundle contents", () => {
  it("opens exactly the seven bundled installers, derived from the release matrix", () => {
    const specs = bundleSpecs(VERSION);
    expect(specs.map((spec) => spec.asset).sort()).toEqual([MAC, DEB, DEB_ARM, MSI, MSI_ARM, NSIS, NSIS_ARM].sort());
    for (const spec of specs) expect(expectedInstallerNames(VERSION)).toContain(spec.asset);
    expect(specs.filter((spec) => spec.arch === "arm64")).toHaveLength(3);
  });

  it("passes when every installer holds every binary, and states what was not opened", async () => {
    const result = check(await bundles());
    expect(result.violations).toEqual([]);
    expect(result.ok).toBe(true);
    expect(result.opened).toHaveLength(7);
    expect(result.coverage).toBe("opened 7 of 12 installers; AppImage, dmg and rpm are not opened");
  });

  it("names the binary and the installer when one is missing from each format", async () => {
    const made = await bundles({
      [MAC]: macTar(...allMacBins("gitpulse-hook")),
      [DEB]: debOf("data.tar.gz", gzipSync(tarball(...BINS.filter((bin) => bin !== "gitpulse-hook").map((bin) => tarEntry(`./usr/bin/${bin}`, elf(0x3e)))))),
      [MSI]: Object.fromEntries(BINS.filter((bin) => bin !== "gitpulse-hook").map((bin) => [bin === "gitpulse" ? "Path" : `Bin_${bin.replaceAll("-", "_")}`, pe(0x8664)])),
      [NSIS_ARM]: Object.fromEntries(BINS.filter((bin) => bin !== "gitpulse-hook").map((bin) => [`${bin}.exe`, pe(0xaa64)])),
    });
    const result = check(made);
    expect(result.ok).toBe(false);
    expect(result.invalid).toBe(false);
    expect(result.violations).toEqual([
      `${MAC}: missing: gitpulse-hook`,
      `${DEB}: missing: gitpulse-hook`,
      `${MSI}: missing: gitpulse-hook`,
      `${NSIS_ARM}: missing: gitpulse-hook`,
    ]);
  });

  it("requires a sixth [[bin]] the moment one is declared", async () => {
    const result = check(await bundles(), [...BINS, "gitpulse-newtool"]);
    expect(result.violations).toHaveLength(7);
    expect(result.violations.every((violation) => violation.endsWith(": missing: gitpulse-newtool"))).toBe(true);
  });

  it("refuses an installer that was not provided rather than skipping it", async () => {
    const made = await bundles();
    const { [DEB]: _omitted, ...rest } = made.files;
    const result = inspectBundleContents({ version: VERSION, binaries: BINS, files: rest, extract: made.extract });
    expect(result.ok).toBe(false);
    expect(result.violations).toEqual([`missing: ${DEB} was not provided for inspection`]);
    expect(result.opened).toHaveLength(6);
  });

  describe("architecture", () => {
    it("fails a thin Mach-O in the universal archive, in either arch", async () => {
      for (const thin of [fat([ARM64]), fat([X86_64]), fat([ARM64, X86_64, 7]), fat([ARM64, ARM64]), elf(0x3e)]) {
        const result = check(await bundles({ [MAC]: macTar(...allMacBins("gitpulsed"), macBin("gitpulsed", thin)) }));
        expect(result.violations, String(thin.subarray(0, 12).toString("hex"))).toEqual([`${MAC}: wrong-arch: gitpulsed`]);
      }
    });

    it("fails an amd64 ELF in the arm64 deb, an arm64 ELF in the amd64 deb, and a non-ELF", async () => {
      const deb = (machine: number, bin = "gitpulsed") => debOf("data.tar.gz", gzipSync(tarball(
        ...BINS.filter((name) => name !== bin).map((name) => tarEntry(`./usr/bin/${name}`, elf(machine === 0x3e ? 0xb7 : 0x3e))), tarEntry(`./usr/bin/${bin}`, elf(machine)))));
      const wrongArm = check(await bundles({ [DEB_ARM]: deb(0x3e) }));
      expect(wrongArm.violations).toEqual([`${DEB_ARM}: wrong-arch: gitpulsed`]);
      const wrongX64 = check(await bundles({ [DEB]: deb(0xb7) }));
      expect(wrongX64.violations).toEqual([`${DEB}: wrong-arch: gitpulsed`]);
      const notElf = check(await bundles({ [DEB]: debOf("data.tar.gz", gzipSync(tarball(...BINS.map((name) => tarEntry(`./usr/bin/${name}`, name === "gitpulsed" ? pe(0x8664) : elf(0x3e)))))) }));
      expect(notElf.violations).toEqual([`${DEB}: wrong-arch: gitpulsed`]);
    });

    it("fails an x64 PE in an arm64 installer and an arm64 PE in an x64 one, in MSI and NSIS", async () => {
      const layout = (machine: number, msi: boolean, bad: number) => Object.fromEntries(BINS.map((bin) =>
        [msi ? (bin === "gitpulse" ? "Path" : `Bin_${bin.replaceAll("-", "_")}`) : `${bin}.exe`, pe(bin === "gitpulsed" ? bad : machine)]));
      const result = check(await bundles({
        [MSI_ARM]: layout(0xaa64, true, 0x8664), [NSIS_ARM]: layout(0xaa64, false, 0x8664),
        [MSI]: layout(0x8664, true, 0xaa64), [NSIS]: layout(0x8664, false, 0xaa64),
      }));
      expect(result.violations).toEqual([NSIS, MSI, NSIS_ARM, MSI_ARM].map((asset) => `${asset}: wrong-arch: gitpulsed`));
    });
  });

  describe("tar entries", () => {
    const only = (entry: Buffer, skip = "gitpulse-hook") => macTar(...allMacBins(skip), entry);

    it("fails a symlink, a hardlink, an empty file, a non-executable file and a directory named as the binary", async () => {
      const cases: [string, Buffer][] = [
        ["symlink", tarEntry("GitPulse.app/Contents/MacOS/gitpulse-hook", Buffer.alloc(0), { type: "2" })],
        ["hardlink", tarEntry("GitPulse.app/Contents/MacOS/gitpulse-hook", Buffer.alloc(0), { type: "1" })],
        ["empty", macBin("gitpulse-hook", Buffer.alloc(0))],
        ["not-executable", macBin("gitpulse-hook", universal(), { mode: 0o644 })],
        ["not-regular", tarEntry("GitPulse.app/Contents/MacOS/gitpulse-hook", Buffer.alloc(0), { type: "5" })],
      ];
      for (const [violation, entry] of cases) {
        expect(check(await bundles({ [MAC]: only(entry) })).violations, violation).toEqual([`${MAC}: ${violation}: gitpulse-hook`]);
      }
    });

    it("fails a binary listed twice, however the second copy is spelled", async () => {
      const dup = check(await bundles({ [MAC]: macTar(...allMacBins(), tarEntry("./GitPulse.app/Contents/MacOS/gitpulse-hook", universal())) }));
      expect(dup.violations).toEqual([`${MAC}: duplicate: gitpulse-hook`]);
    });

    it("fails a path that climbs out of the archive", async () => {
      const result = check(await bundles({ [MAC]: macTar(...allMacBins(), tarEntry("GitPulse.app/../../evil", universal())) }));
      expect(result.violations).toEqual([`${MAC}: unsafe-path: GitPulse.app/../../evil`]);
    });

    it("counts the path an extractor would write: a pax path counts, and a decoy header name does not", async () => {
      const smuggled = paxEntry({ path: "GitPulse.app/Contents/MacOS/gitpulse-hook" }, "decoy", universal());
      expect(check(await bundles({ [MAC]: macTar(...allMacBins("gitpulse-hook"), smuggled) })).violations).toEqual([]);
      const redirected = paxEntry({ path: "somewhere/else" }, "GitPulse.app/Contents/MacOS/gitpulse-hook", universal());
      expect(check(await bundles({ [MAC]: macTar(...allMacBins("gitpulse-hook"), redirected) })).violations).toEqual([`${MAC}: missing: gitpulse-hook`]);
    });

    it("reads a GNU long name", async () => {
      const longName = `GitPulse.app/Contents/MacOS/${"x".repeat(120)}`;
      const entry = Buffer.concat([
        tarEntry("././@LongLink", Buffer.from(`GitPulse.app/Contents/MacOS/gitpulse-hook\0`), { type: "L", mode: 0o644 }),
        tarEntry(longName.slice(0, 100), universal()),
      ]);
      expect(check(await bundles({ [MAC]: macTar(...allMacBins("gitpulse-hook"), entry) })).violations).toEqual([]);
    });

    it("refuses an archive whose sizes lie, whose checksum is wrong, or that never ends", async () => {
      const lying = macTar(...allMacBins("gitpulse-hook"), tarEntry("GitPulse.app/Contents/MacOS/gitpulse-hook", universal(), { declaredSize: 4_000_000 }));
      const corrupt = Buffer.from(tarball(...allMacBins()));
      corrupt[0] ^= 0xff;
      const unterminated = Buffer.concat(allMacBins());
      for (const body of [lying, gzipSync(corrupt), gzipSync(unterminated)]) {
        const result = check(await bundles({ [MAC]: body }));
        expect(result.invalid).toBe(true);
        expect(result.ok).toBe(false);
        expect(result.violations[0]).toContain(`${MAC}: cannot be opened`);
      }
    });

    it("refuses a size on an entry that carries no data", async () => {
      const entry = tarEntry("GitPulse.app/Contents/MacOS/gitpulse-hook", Buffer.alloc(0), { type: "2", declaredSize: 600 });
      const result = check(await bundles({ [MAC]: macTar(...allMacBins("gitpulse-hook"), entry) }));
      expect(result.invalid).toBe(true);
    });

    it("bounds what a gzip stream may unpack to", async () => {
      const result = check(await bundles(), BINS, { maxUnpackedBytes: 2048 });
      expect(result.invalid).toBe(true);
      expect(result.violations.some((violation) => violation.startsWith(`${MAC}: cannot be opened`))).toBe(true);
    });
  });

  describe("deb", () => {
    it("fails closed on a data member it cannot decompress", async () => {
      for (const name of ["data.tar.xz", "data.tar.zst"]) {
        const result = check(await bundles({ [DEB]: debOf(name, Buffer.from("not read")) }));
        expect(result.invalid, name).toBe(true);
        expect(result.violations).toEqual([`${DEB}: cannot be opened: deb data member is ${name}; only data.tar.gz can be opened`]);
      }
    });

    it("refuses a deb with no data member, two of them, or no ar magic", async () => {
      const data = gzipSync(tarball(...BINS.map((bin) => tarEntry(`./usr/bin/${bin}`, elf(0x3e)))));
      const none = Buffer.concat([Buffer.from("!<arch>\n"), arMember("debian-binary", Buffer.from("2.0\n"))]);
      const two = Buffer.concat([debOf("data.tar.gz", data), arMember("data.tar.gz", data)]);
      for (const body of [none, two, Buffer.from("not an archive at all")]) {
        expect(check(await bundles({ [DEB]: body })).invalid).toBe(true);
      }
    });

    it("walks ar members past odd-length padding", async () => {
      const data = gzipSync(tarball(...BINS.map((bin) => tarEntry(`./usr/bin/${bin}`, elf(0x3e)))));
      const odd = Buffer.concat([Buffer.from("!<arch>\n"), arMember("debian-binary", Buffer.from("2.0")), arMember("control.tar.gz", Buffer.from("abc")), arMember("data.tar.gz", data)]);
      expect(check(await bundles({ [DEB]: odd })).violations).toEqual([]);
    });
  });

  describe("windows installers", () => {
    const msiLayout = (change: (layout: Extracted) => void) => {
      const layout: Extracted = Object.fromEntries(BINS.map((bin) => [bin === "gitpulse" ? "Path" : `Bin_${bin.replaceAll("-", "_")}`, pe(0x8664)]));
      change(layout);
      return layout;
    };

    it("fails a symlink, a hardlink, an empty file and a duplicate", async () => {
      const cases: [string, (layout: Extracted) => void][] = [
        ["symlink", (layout) => { layout.Bin_gitpulse_hook = { symlink: "Path" }; }],
        ["hardlink", (layout) => { delete layout.Bin_gitpulse_hook; layout.Spare = pe(0x8664); layout.Bin_gitpulse_hook = { hardlink: "Spare" }; }],
        ["empty", (layout) => { layout.Bin_gitpulse_hook = Buffer.alloc(0); }],
        ["duplicate", (layout) => { layout.Bin_gitpulse_hook_1 = pe(0x8664); }],
        ["wrong-arch", (layout) => { layout.Bin_gitpulse_hook = Buffer.from("MZ not a pe"); }],
      ];
      for (const [violation, change] of cases) {
        const result = check(await bundles({ [MSI]: msiLayout(change) }));
        expect(result.violations, violation).toEqual([`${MSI}: ${violation}: gitpulse-hook`]);
      }
    });

    it("finds NSIS binaries by their own file names and fails a duplicate there too", async () => {
      const layout = (extra: Extracted = {}): Extracted => ({ ...Object.fromEntries(BINS.map((bin) => [`${bin}.exe`, pe(0x8664)])), ...extra });
      expect(check(await bundles({ [NSIS]: layout() })).violations).toEqual([]);
      expect(check(await bundles({ [NSIS]: layout({ "gitpulse-mcp_1.exe": pe(0x8664) }) })).violations).toEqual([`${NSIS}: duplicate: gitpulse-mcp`]);
      const { "gitpulse-ptyd.exe": _gone, ...without } = layout();
      expect(check(await bundles({ [NSIS]: without })).violations).toEqual([`${NSIS}: missing: gitpulse-ptyd`]);
    });

    it("exits through a thrown error, never a pass, when 7z is missing", async () => {
      const made = await bundles();
      const missing = Object.assign(new Error("spawn 7z ENOENT"), { code: "ENOENT" });
      const result = inspectBundleContents({ version: VERSION, binaries: BINS, files: made.files, extract: () => { throw missing; } });
      expect(result.ok).toBe(false);
      expect(result.invalid).toBe(true);
      expect(result.violations).toHaveLength(4);
      expect(result.violations[0]).toContain("cannot be opened: spawn 7z ENOENT");
    });
  });

  it("exits 2 from the CLI when the extraction tool is absent, and 1 when installers are not there", async () => {
    const made = await bundles();
    const env = { ...process.env, PATH: path.join(made.dir, "no-tools") };
    const args = ["--contents", made.dir, "--tag", `v${VERSION}`];
    const noSevenZip = await execFileAsync(process.execPath, [scriptPath, ...args], { env }).catch((error) => error);
    expect(noSevenZip.code).toBe(2);
    expect(String(noSevenZip.stderr)).toContain("7z could not run");
    for (const asset of [MSI, MSI_ARM, NSIS, NSIS_ARM]) await rm(path.join(made.dir, asset));
    const absent = await execFileAsync(process.execPath, [scriptPath, ...args], { env }).catch((error) => error);
    expect(absent.code).toBe(1);
    expect(String(absent.stderr)).toContain(`missing: ${MSI} was not provided`);
    expect(String(absent.stdout)).toContain("opened 3 of 12 installers");
  });
});

/**
 * Adversarial cases for ft-ac3bba92 (adversary pass, build run 2026-10-26).
 * Fixtures for the same cases, run through the CLI, are under
 * tmp/build-1026/l2-adv/. Each `it` says whether it broke the implementation
 * (`BREAKS`) or documents a case that held (`HOLDS`).
 */
describe("bundle contents: adversarial bundles", () => {
  /** What real v1.3.1 Windows installers extract to: every binary at the root of the tree (checked with 7z 23.01 on both). */
  const windowsExtract = (place: (bin: string, id: string) => string, extraFile?: (outDir: string) => void) => (archive: string, outDir: string) => {
    const msi = archive.endsWith(".msi");
    const machine = archive.includes("arm64") ? 0xaa64 : 0x8664;
    for (const bin of BINS) {
      const id = msi ? (bin === "gitpulse" ? "Path" : `Bin_${bin.replaceAll("-", "_")}`) : `${bin}.exe`;
      const dest = path.join(outDir, place(bin, id));
      mkdirSync(path.dirname(dest), { recursive: true });
      writeFileSync(dest, pe(machine));
    }
    extraFile?.(outDir);
  };
  const runWindows = async (extract: (archive: string, outDir: string) => void, extra: Record<string, unknown> = {}) =>
    inspectBundleContents({ version: VERSION, binaries: BINS, files: (await bundles()).files, extract, ...extra });

  /** A header with a ustar `prefix`, the way GNU tar splits a path longer than 100 bytes. */
  function prefixedEntry(prefix: string, name: string, data: Buffer): Buffer {
    const header = Buffer.from(tarHeader(name, data.length));
    header.write(prefix, 345, 155, "utf8");
    header.write("        ", 148, "latin1");
    let sum = 0;
    for (const byte of header) sum += byte;
    header.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148, "latin1");
    return Buffer.concat([header, padded(data)]);
  }

  describe("a symlinked ancestor directory (BREAKS)", () => {
    // An extractor writes a regular file named under a symlinked directory
    // through the link, so the installed binary does not live in the app.
    it.each([
      ["the macOS MacOS directory", "GitPulse.app/Contents/MacOS"],
      ["the macOS Contents directory", "GitPulse.app/Contents"],
    ])("fails a bundle whose binaries sit under %s when it is a symlink", async (_label, link) => {
      const result = check(await bundles({ [MAC]: macTar(tarEntry(link, Buffer.alloc(0), { type: "2" }), ...allMacBins()) }));
      expect(result.ok).toBe(false);
    });

    it("fails a deb whose usr/bin is a symlink", async () => {
      const link = tarEntry("./usr/bin", Buffer.alloc(0), { type: "2" });
      const deb = debOf("data.tar.gz", gzipSync(tarball(link, ...BINS.map((bin) => tarEntry(`./usr/bin/${bin}`, elf(0x3e))))));
      const result = check(await bundles({ [DEB]: deb }));
      expect(result.ok).toBe(false);
    });
  });

  describe("a binary only in the wrong place in a Windows installer (BREAKS)", () => {
    // 7-Zip lays both real v1.3.1 installers out with every binary at the
    // root; an NSIS `$PLUGINSDIR` copy is a temp file the installer never installs.
    it("accepts the layout the real installers have: every binary at the root of the extracted tree", async () => {
      const result = await runWindows(windowsExtract((_bin, id) => id));
      expect(result.violations).toEqual([]);
    });

    it.each([
      ["NSIS", NSIS, "$PLUGINSDIR"],
      ["MSI", MSI, "somewhere/else"],
    ])("fails %s when gitpulse-hook is found only inside a subdirectory", async (_label, asset, dir) => {
      const result = await runWindows(windowsExtract((bin, id) => (bin === "gitpulse-hook" ? `${dir}/${id}` : id)));
      expect(result.ok).toBe(false);
      expect(result.violations.some((violation) => violation.startsWith(`${asset}:`) && violation.includes("gitpulse-hook"))).toBe(true);
    });
  });

  it("bounds what 7z may unpack, not only what gunzip may (BREAKS)", async () => {
    // maxUnpackedBytes is the one extraction ceiling, but only the gzip path applies it.
    const extract = windowsExtract((_bin, id) => id, (outDir) => {
      const big = path.join(outDir, "big.dat");
      writeFileSync(big, Buffer.alloc(1));
      truncateSync(big, 10 * 1024 * 1024);
    });
    const result = await runWindows(extract, { maxUnpackedBytes: 64 * 1024 });
    expect(result.ok).toBe(false);
  });

  it("does not pass a fat Mach-O whose slices lie past the end of the file (BREAKS)", async () => {
    const lying = universal();
    lying.writeUInt32BE(0x4000, 16);
    lying.writeUInt32BE(0x4000_0000, 20);
    lying.writeUInt32BE(0x8000, 36);
    lying.writeUInt32BE(0x4000_0000, 40);
    const result = check(await bundles({ [MAC]: macTar(...allMacBins("gitpulsed"), macBin("gitpulsed", lying)) }));
    expect(result.ok).toBe(false);
  });

  describe("declaredBinaryNames never drops a [[bin]] quietly (BREAKS)", () => {
    // Either spelling is fine: read the name, or throw. A silent omission
    // makes that binary optional in every bundle.
    it.each([
      ["a TOML literal string", "[[bin]]\nname = 'alpha'\npath = 'a.rs'\n"],
      ["whitespace inside the table header", '[[ bin ]]\nname = "alpha"\n'],
      ["an inline bin array", 'bin = [{ name = "alpha", path = "a.rs" }]\n'],
    ])("reads %s or refuses it", (_label, toml) => {
      let names: string[];
      try {
        names = declaredBinaryNames(toml);
      } catch {
        return;
      }
      expect(names).toContain("alpha");
    });
  });

  describe("cases that hold", () => {
    it("HOLDS: a suffixed, prefixed or wrongly placed copy of a binary does not stand in for it", async () => {
      for (const entry of [
        macBin("gitpulse-hook.bak"),
        macBin("xgitpulse-hook"),
        tarEntry("GitPulse.app/Contents/Resources/gitpulse-hook", universal()),
        tarEntry("GitPulse.app//Contents/MacOS/gitpulse-hook", universal()),
      ]) {
        const result = check(await bundles({ [MAC]: macTar(...allMacBins("gitpulse-hook"), entry) }));
        expect(result.violations).toEqual([`${MAC}: missing: gitpulse-hook`]);
      }
    });

    it("HOLDS: a path with spaces and non-ASCII names, and a ustar prefix split, are read", async () => {
      const body = macTar(
        ...allMacBins("gitpulse-hook"),
        prefixedEntry("GitPulse.app/Contents", "MacOS/gitpulse-hook", universal()),
        tarEntry("GitPulse.app/Contents/Resources/é file 日本.txt", Buffer.from("x"), { mode: 0o644 }),
      );
      expect(check(await bundles({ [MAC]: body })).violations).toEqual([]);
    });

    it("HOLDS: a truncated gzip, a corrupted gzip and an uncompressed tar are refused as unreadable", async () => {
      const good = macTar(...allMacBins());
      const flipped = Buffer.from(good);
      flipped[Math.floor(good.length / 2)] ^= 0xff;
      for (const body of [good.subarray(0, Math.floor(good.length / 2)), flipped, tarball(...allMacBins())]) {
        const result = check(await bundles({ [MAC]: body }));
        expect(result.invalid).toBe(true);
        expect(result.ok).toBe(false);
      }
    });

    it("HOLDS: a bundle that is zero bytes is refused as unreadable, not as missing", async () => {
      const result = check(await bundles({ [MAC]: Buffer.alloc(0) }));
      expect(result.invalid).toBe(true);
      expect(result.violations[0]).toContain("bundle size 0");
    });

    it("HOLDS: a 7z that exits 0 and extracts nothing is a refusal for every binary", async () => {
      const result = await runWindows(() => undefined);
      expect(result.ok).toBe(false);
      expect(result.violations.filter((violation) => violation.startsWith(NSIS))).toHaveLength(BINS.length);
    });

    it.skipIf(process.platform === "win32")("HOLDS: the scratch directory is removed whether the extractor succeeds, fails or throws", async () => {
      const made = await bundles();
      const scratch = mkdtempSync(path.join(tmpdir(), "gitpulse-adv-tmp-"));
      tempDirs.push(scratch);
      const previous = process.env.TMPDIR;
      process.env.TMPDIR = scratch;
      try {
        inspectBundleContents({ version: VERSION, binaries: BINS, files: made.files, extract: made.extract });
        inspectBundleContents({ version: VERSION, binaries: BINS, files: made.files, extract: () => { throw new Error("7z exited 2"); } });
        inspectBundleContents({
          version: VERSION, binaries: BINS, files: made.files,
          extract: (_archive, outDir) => { writeFileSync(path.join(outDir, "partial"), "x"); throw new Error("7z hung"); },
        });
      } finally {
        if (previous === undefined) delete process.env.TMPDIR;
        else process.env.TMPDIR = previous;
      }
      expect(readdirSync(scratch)).toEqual([]);
    });
  });
});
