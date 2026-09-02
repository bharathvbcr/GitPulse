import { describe, expect, it } from "vitest";
import {
  resolveLanguageIconKey,
  getLanguageBrandColor,
  getLanguageDisplayName,
} from "./languageLogos";

describe("languageLogos resolver", () => {
  it("resolves from common language names", () => {
    expect(resolveLanguageIconKey("Rust")).toBe("rust");
    expect(resolveLanguageIconKey("TypeScript")).toBe("typescript");
    expect(resolveLanguageIconKey("JavaScript")).toBe("javascript");
    expect(resolveLanguageIconKey("Python")).toBe("python");
    expect(resolveLanguageIconKey("Go")).toBe("go");
    expect(resolveLanguageIconKey("Svelte")).toBe("svelte");
    expect(resolveLanguageIconKey("HTML")).toBe("html");
    expect(resolveLanguageIconKey("CSS")).toBe("css");
    expect(resolveLanguageIconKey("C")).toBe("c");
    expect(resolveLanguageIconKey("C++")).toBe("cpp");
    expect(resolveLanguageIconKey("C#")).toBe("csharp");
    expect(resolveLanguageIconKey("Java")).toBe("java");
    expect(resolveLanguageIconKey("Ruby")).toBe("ruby");
    expect(resolveLanguageIconKey("PHP")).toBe("php");
    expect(resolveLanguageIconKey("Swift")).toBe("swift");
    expect(resolveLanguageIconKey("Kotlin")).toBe("kotlin");
    expect(resolveLanguageIconKey("Shell")).toBe("shell");
    expect(resolveLanguageIconKey("SQL")).toBe("sql");
    expect(resolveLanguageIconKey("JSON")).toBe("json");
    expect(resolveLanguageIconKey("YAML")).toBe("yaml");
    expect(resolveLanguageIconKey("TOML")).toBe("toml");
    expect(resolveLanguageIconKey("Markdown")).toBe("markdown");
    expect(resolveLanguageIconKey("Docker")).toBe("docker");
  });

  it("resolves from file paths and filenames", () => {
    expect(resolveLanguageIconKey("src-tauri/src/main.rs")).toBe("rust");
    expect(resolveLanguageIconKey("src/App.svelte")).toBe("svelte");
    expect(resolveLanguageIconKey("index.ts")).toBe("typescript");
    expect(resolveLanguageIconKey("Component.tsx")).toBe("typescript");
    expect(resolveLanguageIconKey("server.js")).toBe("javascript");
    expect(resolveLanguageIconKey("App.jsx")).toBe("javascript");
    expect(resolveLanguageIconKey("main.py")).toBe("python");
    expect(resolveLanguageIconKey("cmd/main.go")).toBe("go");
    expect(resolveLanguageIconKey("Dockerfile")).toBe("docker");
    expect(resolveLanguageIconKey("docker-compose.yml")).toBe("docker");
    expect(resolveLanguageIconKey(".gitignore")).toBe("git");
    expect(resolveLanguageIconKey("Cargo.lock")).toBe("lock");
    expect(resolveLanguageIconKey("package-lock.json")).toBe("lock");
    expect(resolveLanguageIconKey("icon.svg")).toBe("svg");
    expect(resolveLanguageIconKey("banner.png")).toBe("image");
  });

  it("resolves brand colors and display names", () => {
    expect(getLanguageBrandColor("rust")).toBe("#dea584");
    expect(getLanguageBrandColor("typescript")).toBe("#3178c6");
    expect(getLanguageDisplayName("rust")).toBe("Rust");
    expect(getLanguageDisplayName("typescript")).toBe("TypeScript");
  });

  it("handles unknown or empty inputs gracefully", () => {
    expect(resolveLanguageIconKey("")).toBe("file");
    expect(resolveLanguageIconKey("unknown_file.xyz")).toBe("file");
    expect(getLanguageBrandColor("file")).toBe("#6b7280");
  });
});
