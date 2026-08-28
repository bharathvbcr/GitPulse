/**
 * File icon and category color mappings for the GitPulse IDE File Explorer.
 */

export interface FileIconMeta {
  colorClass: string;
  badgeLabel: string;
  isImage?: boolean;
}

export function getFileIconMeta(filePath: string): FileIconMeta {
  if (!filePath) {
    return { colorClass: "text-textMuted", badgeLabel: "TXT" };
  }

  const lower = filePath.toLowerCase();
  const name = lower.slice(lower.lastIndexOf("/") + 1);
  const ext = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1) : "";

  // Special full filenames
  if (name === "dockerfile" || name.startsWith("dockerfile.")) {
    return { colorClass: "text-sky-400", badgeLabel: "DOCKER" };
  }
  if (name === "package.json" || name === "cargo.toml" || name === "go.mod" || name === "pyproject.toml") {
    return { colorClass: "text-amber-400", badgeLabel: "CONF" };
  }
  if (name.includes("lock") || name.endsWith(".lock")) {
    return { colorClass: "text-textMuted", badgeLabel: "LOCK" };
  }
  if (name.startsWith(".git")) {
    return { colorClass: "text-rose-400", badgeLabel: "GIT" };
  }

  // Extensions
  switch (ext) {
    case "ts":
    case "mts":
    case "cts":
      return { colorClass: "text-blue-400", badgeLabel: "TS" };
    case "tsx":
      return { colorClass: "text-blue-400", badgeLabel: "TSX" };
    case "js":
    case "mjs":
    case "cjs":
      return { colorClass: "text-yellow-400", badgeLabel: "JS" };
    case "jsx":
      return { colorClass: "text-yellow-400", badgeLabel: "JSX" };
    case "rs":
      return { colorClass: "text-orange-400", badgeLabel: "RS" };
    case "svelte":
      return { colorClass: "text-rose-500", badgeLabel: "SVELTE" };
    case "html":
    case "htm":
      return { colorClass: "text-orange-500", badgeLabel: "HTML" };
    case "css":
    case "scss":
    case "sass":
    case "less":
      return { colorClass: "text-indigo-400", badgeLabel: "CSS" };
    case "json":
    case "jsonc":
    case "json5":
      return { colorClass: "text-amber-400", badgeLabel: "JSON" };
    case "yaml":
    case "yml":
      return { colorClass: "text-emerald-400", badgeLabel: "YAML" };
    case "md":
    case "markdown":
    case "mdx":
      return { colorClass: "text-sky-300", badgeLabel: "MD" };
    case "py":
      return { colorClass: "text-blue-500", badgeLabel: "PY" };
    case "go":
      return { colorClass: "text-cyan-400", badgeLabel: "GO" };
    case "sh":
    case "bash":
    case "zsh":
      return { colorClass: "text-emerald-500", badgeLabel: "SH" };
    case "c":
    case "h":
      return { colorClass: "text-blue-300", badgeLabel: "C" };
    case "cpp":
    case "hpp":
    case "cc":
      return { colorClass: "text-blue-400", badgeLabel: "CPP" };
    case "sql":
      return { colorClass: "text-purple-400", badgeLabel: "SQL" };
    case "toml":
    case "ini":
    case "env":
      return { colorClass: "text-zinc-400", badgeLabel: "CFG" };
    case "png":
    case "jpg":
    case "jpeg":
    case "gif":
    case "svg":
    case "webp":
    case "ico":
    case "avif":
      return { colorClass: "text-teal-400", badgeLabel: "IMG", isImage: true };
    default:
      return { colorClass: "text-textMuted/80", badgeLabel: ext.slice(0, 4).toUpperCase() || "FILE" };
  }
}
