import { describe, expect, it } from "vitest";
import {
  logoDataUrl,
  logoDirOf,
  logoFolderView,
  logoImagePaths,
  logoInitials,
  logoParentDir,
  monogramColor,
  rankLogoCandidates,
  scoreLogoPath,
} from "./repoLogo";
import { TAB_COLORS } from "./tabColors";

const PNG_HEAD = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d];
function b64(bytes: number[]): string {
  return btoa(String.fromCharCode(...bytes));
}

describe("logoImagePaths", () => {
  it("keeps every drawable image, wherever it sits, in path order and once", () => {
    const listing = [
      "src/main.rs",
      "tests/fixtures/logo.png",
      "assets\\icon10.png",
      "assets/icon2.png",
      "assets/icon2.png",
      "/README.md",
      "docs/",
      "",
      "a\0b.png",
      "x".repeat(2000) + ".png",
      "Logo.SVG",
    ];
    expect(logoImagePaths(listing)).toEqual({
      paths: ["assets/icon2.png", "assets/icon10.png", "Logo.SVG", "tests/fixtures/logo.png"],
      total: 4,
    });
  });

  it("caps what it keeps but counts everything", () => {
    const listing = Array.from({ length: 50 }, (_, i) => `img/${i}.png`);
    const list = logoImagePaths(listing, 10);
    expect(list.paths).toHaveLength(10);
    expect(list.total).toBe(50);
    expect(logoImagePaths(listing, -1).paths).toEqual([]);
  });
});

describe("logoFolderView", () => {
  const images = [
    "logo.svg",
    "assets/brand/logo-dark.png",
    "assets/brand/logo.png",
    "assets/banner.png",
    "src-tauri/icons/128x128.png",
    "src-tauri/icons/32x32.png",
    "docs/img/screens/one.png",
  ];

  it("lists one folder: its subfolders with image counts, then its files", () => {
    const root = logoFolderView(images, "");
    expect(root.folders).toEqual([
      { name: "assets", path: "assets", images: 3 },
      { name: "docs/img/screens", path: "docs/img/screens", images: 1 },
      { name: "src-tauri/icons", path: "src-tauri/icons", images: 2 },
    ]);
    expect(root.files).toEqual(["logo.svg"]);
    expect(root.total).toBe(4);
    const assets = logoFolderView(images, "assets");
    expect(assets.folders).toEqual([{ name: "brand", path: "assets/brand", images: 2 }]);
    expect(assets.files).toEqual(["assets/banner.png"]);
    // A trailing slash and an empty folder read the same as without.
    expect(logoFolderView(images, "assets/")).toEqual(assets);
    expect(logoFolderView(images, "nowhere")).toEqual({ folders: [], files: [], total: 0 });
  });

  it("does not take a sibling whose name only starts the same for a subfolder", () => {
    const view = logoFolderView(["app/a.png", "apple/b.png"], "app");
    expect(view.files).toEqual(["app/a.png"]);
    expect(view.folders).toEqual([]);
  });

  it("searches below the folder by every word, names before paths, logos first", () => {
    const all = logoFolderView(images, "", "logo");
    expect(all.folders).toEqual([]);
    expect(all.files[0]).toBe("logo.svg");
    expect(all.files).toContain("assets/brand/logo.png");
    expect(all.files).toContain("assets/brand/logo-dark.png");
    expect(all.files).not.toContain("assets/banner.png");
    expect(logoFolderView(images, "assets", "LOGO dark").files).toEqual(["assets/brand/logo-dark.png"]);
    // Below the folder only: the root's logo.svg is not under assets/.
    expect(logoFolderView(images, "assets", "logo").files).not.toContain("logo.svg");
    expect(logoFolderView(images, "", "   ").folders.length).toBeGreaterThan(0);
  });

  it("caps the entries shown, folders first, and reports the full count", () => {
    const many = [...Array.from({ length: 5 }, (_, i) => `f${i}/x.png`), ...Array.from({ length: 5 }, (_, i) => `${i}.png`)];
    const view = logoFolderView(many, "", "", 7);
    expect(view.folders).toHaveLength(5);
    expect(view.files).toHaveLength(2);
    expect(view.total).toBe(10);
    const found = logoFolderView(many, "", "png", 3);
    expect(found.files).toHaveLength(3);
    expect(found.total).toBe(10);
  });

  it("goes up past folders the view collapses", () => {
    expect(logoParentDir(images, "src-tauri/icons")).toBe("");
    expect(logoParentDir(images, "docs/img/screens")).toBe("");
    expect(logoParentDir(images, "assets/brand")).toBe("assets");
    expect(logoParentDir(images, "assets")).toBe("");
    expect(logoParentDir(images, "")).toBe("");
    expect(logoParentDir(["a/b/x.png", "a/y.png"], "a/b")).toBe("a");
  });

  it("names the folder a path sits in", () => {
    expect(logoDirOf("logo.png")).toBe("");
    expect(logoDirOf("a/b/logo.png")).toBe("a/b");
  });
});

describe("rankLogoCandidates", () => {
  it("prefers a small Tauri icon over a sub-project's logo (GitPulse's own tree)", () => {
    const listing = [
      "plugins/gitpulse-agy/assets/logo.svg",
      "plugins/gitpulse/assets/logo.png",
      "src-tauri/icons/icon.ico",
      "src-tauri/icons/icon.png",
      "src-tauri/icons/32x32.png",
      "src-tauri/icons/128x128.png",
      "src-tauri/icons/Square150x150Logo.png",
      "src/lib/main.ts",
    ];
    expect(rankLogoCandidates(listing, 1)).toEqual(["src-tauri/icons/128x128.png"]);
  });

  it("puts a root logo first and a project's art folder ahead of deep source trees", () => {
    const listing = ["src/components/ui/logo.svg", "assets/logo.png", "logo.svg"];
    expect(rankLogoCandidates(listing, 3)).toEqual(["logo.svg", "assets/logo.png", "src/components/ui/logo.svg"]);
  });

  it("takes JetBrains' .idea/icon.svg, the explicit project-icon convention, over everything", () => {
    expect(rankLogoCandidates(["logo.svg", ".idea/icon.svg"], 1)).toEqual([".idea/icon.svg"]);
  });

  it("never offers dependency, build or test images", () => {
    const listing = [
      "node_modules/pkg/logo.svg",
      "vendor/lib/logo.png",
      "dist/logo.png",
      "tests/fixtures/logo.png",
      "src/__tests__/icon.png",
      "target/release/icon.png",
    ];
    expect(rankLogoCandidates(listing, 10)).toEqual([]);
  });

  it("refuses marketing art and non-images unless any image is asked for", () => {
    expect(scoreLogoPath("docs/logo-banner.png")).toBeNull();
    expect(scoreLogoPath("README.md")).toBeNull();
    expect(scoreLogoPath("docs/screenshot.png")).toBeNull();
    expect(scoreLogoPath("docs/screenshot.png", { anyImage: true })).not.toBeNull();
    expect(scoreLogoPath("logo")).toBeNull();
  });

  it("ranks a plain logo above its theme variants", () => {
    expect(rankLogoCandidates(["assets/logo-dark.svg", "assets/logo.svg"], 2)).toEqual(["assets/logo.svg", "assets/logo-dark.svg"]);
  });

  it("finds an Android launcher icon and an Xcode AppIcon set", () => {
    expect(rankLogoCandidates(["app/src/main/res/mipmap-xxhdpi/ic_launcher.png", "README.md"], 1)).toEqual([
      "app/src/main/res/mipmap-xxhdpi/ic_launcher.png",
    ]);
    expect(rankLogoCandidates(["App/Assets.xcassets/AppIcon.appiconset/icon_128.png"], 1)).toEqual([
      "App/Assets.xcassets/AppIcon.appiconset/icon_128.png",
    ]);
  });

  it("is stable for ties and bounded by the limit", () => {
    expect(rankLogoCandidates(["b/logo.png", "a/logo.png"], 2)).toEqual(["b/logo.png", "a/logo.png"]);
    expect(rankLogoCandidates(["logo.png", "icon.png", "favicon.ico"], 0)).toEqual([]);
  });

  it("accepts Windows separators and skips junk entries", () => {
    const junk = ["", "x".repeat(2000) + ".png", "assets\\logo.png"] as string[];
    expect(rankLogoCandidates(junk, 5)).toEqual(["assets/logo.png"]);
  });

  it("picks published *-mark assets (such as Manvi's) without beating GitPulse's Tauri icon or false-matching words", () => {
    expect(
      rankLogoCandidates(["assets/manvi-mark.png", "assets/manvi-mark.svg", "docs/screenshot.png", "README.md"], 2),
    ).toEqual(["assets/manvi-mark.svg", "assets/manvi-mark.png"]);
    expect(
      rankLogoCandidates(["public/waylode-mark.svg", "art/brand/mark.png", "src-tauri/icons/128x128.png"], 1),
    ).toEqual(["src-tauri/icons/128x128.png"]);
    for (const nonLogo of ["benchmark.png", "bookmark.png", "watermark.png", "markdown.png"]) {
      expect(scoreLogoPath(nonLogo)).toBeNull();
    }
  });
});

describe("logoDataUrl", () => {
  it("builds a data URL for a PNG whose bytes are a PNG", () => {
    const base64 = b64(PNG_HEAD);
    expect(logoDataUrl("icon.png", { base64 })).toBe(`data:image/png;base64,${base64}`);
  });

  it("refuses bytes that are not the format the name claims", () => {
    expect(logoDataUrl("icon.png", { base64: b64([0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0]) })).toBeNull();
    expect(logoDataUrl("icon.png", { base64: "not base64!!" })).toBeNull();
    expect(logoDataUrl("icon.png", { text: "plain" })).toBeNull();
    expect(logoDataUrl("notes.txt", { base64: b64(PNG_HEAD) })).toBeNull();
  });

  it("allows a raster image larger than 512 KiB", () => {
    const large = b64(PNG_HEAD) + "A".repeat(700_000);
    expect(logoDataUrl("icon.png", { base64: large })).toBe(`data:image/png;base64,${large}`);
  });

  it("encodes an SVG read as text, including ones over 512 KiB, and refuses text that is not an SVG", () => {
    const svg = '<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"><title>é</title></svg>';
    const url = logoDataUrl("logo.svg", { text: svg });
    expect(url?.startsWith("data:image/svg+xml;base64,")).toBe(true);
    const decoded = new TextDecoder().decode(Uint8Array.from(atob(url!.split(",")[1]), (c) => c.charCodeAt(0)));
    expect(decoded).toBe(svg);
    expect(logoDataUrl("logo.svg", { text: "<html><script>alert(1)</script></html>" })).toBeNull();
    const largeSvg = `<svg xmlns="http://www.w3.org/2000/svg"><!-- ${"x".repeat(600 * 1024)} --></svg>`;
    const largeUrl = logoDataUrl("logo.svg", { text: largeSvg });
    expect(largeUrl?.startsWith("data:image/svg+xml;base64,")).toBe(true);
    const decodedLarge = new TextDecoder().decode(
      Uint8Array.from(atob(largeUrl!.split(",")[1]), (c) => c.charCodeAt(0)),
    );
    expect(decodedLarge).toBe(largeSvg);
  });

  it("knows the other image signatures", () => {
    expect(logoDataUrl("a.gif", { base64: b64([...Array.from("GIF89a", (c) => c.charCodeAt(0)), 0, 0, 0, 0, 0, 0]) })).toMatch(/^data:image\/gif/);
    expect(logoDataUrl("a.ico", { base64: b64([0, 0, 1, 0, 1, 0, 16, 16, 0, 0, 0, 0]) })).toMatch(/^data:image\/x-icon/);
    const webp = [..."RIFF"].map((c) => c.charCodeAt(0)).concat([0, 0, 0, 0], [..."WEBP"].map((c) => c.charCodeAt(0)));
    expect(logoDataUrl("a.webp", { base64: b64(webp) })).toMatch(/^data:image\/webp/);
  });
});

describe("logoInitials", () => {
  it("takes the first letters of the first two words", () => {
    expect(logoInitials("GitPulse")).toBe("GP");
    expect(logoInitials("DevCouncil")).toBe("DC");
    expect(logoInitials("dev-council")).toBe("DC");
    expect(logoInitials("my_repo.git")).toBe("MR");
    expect(logoInitials("gusset")).toBe("G");
    expect(logoInitials("  ")).toBe("?");
    expect(logoInitials("ångström")).toBe("Å");
  });
});

describe("monogramColor", () => {
  it("uses the tab's own color, else a stable palette color by name", () => {
    expect(monogramColor("GitPulse", "teal")).toBe("teal");
    const a = monogramColor("GitPulse", null);
    expect(TAB_COLORS).toContain(a);
    expect(monogramColor("gitpulse", undefined)).toBe(a);
  });
});
