/**
 * Language logo and file type identifiers for GitPulse.
 * Maps programming languages, file extensions, and special filenames
 * to canonical icon keys and official brand colors.
 */

export type LanguageIconKey =
  | "rust"
  | "typescript"
  | "javascript"
  | "python"
  | "go"
  | "svelte"
  | "html"
  | "css"
  | "c"
  | "cpp"
  | "csharp"
  | "java"
  | "ruby"
  | "php"
  | "swift"
  | "kotlin"
  | "dart"
  | "shell"
  | "sql"
  | "lua"
  | "zig"
  | "json"
  | "yaml"
  | "toml"
  | "markdown"
  | "xml"
  | "svg"
  | "image"
  | "docker"
  | "git"
  | "lock"
  | "archive"
  | "config"
  | "file";

const LANGUAGE_NAME_MAP: Record<string, LanguageIconKey> = {
  rust: "rust",
  typescript: "typescript",
  javascript: "javascript",
  python: "python",
  go: "go",
  golang: "go",
  svelte: "svelte",
  html: "html",
  css: "css",
  c: "c",
  "c++": "cpp",
  cpp: "cpp",
  "c#": "csharp",
  csharp: "csharp",
  java: "java",
  ruby: "ruby",
  php: "php",
  swift: "swift",
  kotlin: "kotlin",
  dart: "dart",
  shell: "shell",
  bash: "shell",
  zsh: "shell",
  sh: "shell",
  sql: "sql",
  lua: "lua",
  zig: "zig",
  json: "json",
  yaml: "yaml",
  toml: "toml",
  markdown: "markdown",
  xml: "xml",
  docker: "docker",
  dockerfile: "docker",
  git: "git",
  image: "image",
};

const EXTENSION_MAP: Record<string, LanguageIconKey> = {
  rs: "rust",
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  tsx: "typescript",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "javascript",
  py: "python",
  pyw: "python",
  ipynb: "python",
  go: "go",
  svelte: "svelte",
  html: "html",
  htm: "html",
  css: "css",
  scss: "css",
  sass: "css",
  less: "css",
  c: "c",
  h: "c",
  cpp: "cpp",
  hpp: "cpp",
  cc: "cpp",
  cxx: "cpp",
  hh: "cpp",
  cs: "csharp",
  java: "java",
  jar: "java",
  rb: "ruby",
  php: "php",
  swift: "swift",
  kt: "kotlin",
  kts: "kotlin",
  dart: "dart",
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  sql: "sql",
  lua: "lua",
  zig: "zig",
  json: "json",
  jsonc: "json",
  json5: "json",
  yaml: "yaml",
  yml: "yaml",
  toml: "toml",
  md: "markdown",
  markdown: "markdown",
  mdx: "markdown",
  xml: "xml",
  svg: "svg",
  png: "image",
  jpg: "image",
  jpeg: "image",
  gif: "image",
  webp: "image",
  ico: "image",
  avif: "image",
  zip: "archive",
  tar: "archive",
  gz: "archive",
  "7z": "archive",
  rar: "archive",
  ini: "config",
  env: "config",
  cfg: "config",
  conf: "config",
};

const BRAND_COLORS: Record<LanguageIconKey, string> = {
  rust: "#dea584",
  typescript: "#3178c6",
  javascript: "#f7df1e",
  python: "#3776ab",
  go: "#00add8",
  svelte: "#ff3e00",
  html: "#e34f26",
  css: "#1572b6",
  c: "#555555",
  cpp: "#f34b7d",
  csharp: "#178600",
  java: "#b07219",
  ruby: "#701516",
  php: "#4f5d95",
  swift: "#f05138",
  kotlin: "#a97bff",
  dart: "#00b4ab",
  shell: "#89e051",
  sql: "#e38c00",
  lua: "#000080",
  zig: "#ec915c",
  json: "#cbcb41",
  yaml: "#cb171e",
  toml: "#9c4221",
  markdown: "#083fa1",
  xml: "#0060ac",
  svg: "#ff9900",
  image: "#a2d9ff",
  docker: "#384d54",
  git: "#f05032",
  lock: "#6b7280",
  archive: "#d48434",
  config: "#6e7681",
  file: "#6b7280",
};

const DISPLAY_NAMES: Record<LanguageIconKey, string> = {
  rust: "Rust",
  typescript: "TypeScript",
  javascript: "JavaScript",
  python: "Python",
  go: "Go",
  svelte: "Svelte",
  html: "HTML",
  css: "CSS",
  c: "C",
  cpp: "C++",
  csharp: "C#",
  java: "Java",
  ruby: "Ruby",
  php: "PHP",
  swift: "Swift",
  kotlin: "Kotlin",
  dart: "Dart",
  shell: "Shell",
  sql: "SQL",
  lua: "Lua",
  zig: "Zig",
  json: "JSON",
  yaml: "YAML",
  toml: "TOML",
  markdown: "Markdown",
  xml: "XML",
  svg: "SVG",
  image: "Image",
  docker: "Docker",
  git: "Git",
  lock: "Lockfile",
  archive: "Archive",
  config: "Config",
  file: "File",
};

/**
 * Resolves an icon key from a language name, full path, or file extension.
 */
export function resolveLanguageIconKey(input: string): LanguageIconKey {
  if (!input) return "file";

  const clean = input.trim().toLowerCase();

  // 1. Direct language name match (e.g. from Rust analyzer or LanguageBar)
  if (LANGUAGE_NAME_MAP[clean]) {
    return LANGUAGE_NAME_MAP[clean];
  }

  // 2. Extract filename and extension
  const normalized = clean.replace(/\\/g, "/");
  const filename = normalized.slice(normalized.lastIndexOf("/") + 1);

  // Special full filenames
  if (
    filename === "dockerfile" ||
    filename.startsWith("dockerfile.") ||
    filename.startsWith("docker-compose.") ||
    filename === "compose.yaml" ||
    filename === "compose.yml"
  ) {
    return "docker";
  }
  if (filename.startsWith(".git")) {
    return "git";
  }
  if (
    filename === "cargo.lock" ||
    filename === "package-lock.json" ||
    filename === "yarn.lock" ||
    filename === "pnpm-lock.yaml" ||
    filename === "bun.lock" ||
    filename === "bun.lockb" ||
    filename.endsWith(".lock")
  ) {
    return "lock";
  }

  const dotIdx = filename.lastIndexOf(".");
  if (dotIdx > 0 && dotIdx < filename.length - 1) {
    const ext = filename.slice(dotIdx + 1);
    if (EXTENSION_MAP[ext]) {
      return EXTENSION_MAP[ext];
    }
  }

  return "file";
}

export function getLanguageBrandColor(key: LanguageIconKey): string {
  return BRAND_COLORS[key] ?? BRAND_COLORS.file;
}

export function getLanguageDisplayName(key: LanguageIconKey): string {
  return DISPLAY_NAMES[key] ?? DISPLAY_NAMES.file;
}
