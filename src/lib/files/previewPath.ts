import { resolveLinkAction, resolveRepoFile } from "../terminal/links";

export const ASSET_SCHEME_RE = /^[A-Za-z][A-Za-z0-9+.-]*:/;
export const EXTERNAL_RESOURCE_URL_RE = /^(?:https?:|ftp:|file:|\/\/)/i;

const UTF8_ENCODER = typeof TextEncoder !== "undefined" ? new TextEncoder() : null;

export function utf8ByteLength(text: string): number {
  if (!text) return 0;
  return UTF8_ENCODER ? UTF8_ENCODER.encode(text).byteLength : text.length;
}

export function normalizeRepoRelativePath(path: string): string {
  return path.trim().replace(/\\/g, "/").replace(/^\.\//, "");
}

export function isLocalAssetRef(raw: string): boolean {
  const value = raw.trim();
  if (!value || value.startsWith("#")) return false;
  if (EXTERNAL_RESOURCE_URL_RE.test(value)) return false;
  if (ASSET_SCHEME_RE.test(value) || value.startsWith("\\")) return false;
  return true;
}

/**
 * Resolves a relative asset reference inside an HTML, CSS, or SVG file (`./style.css`,
 * `../img/logo.png`, `/assets/site.css`) to a repo-relative POSIX path, or
 * `null` if it escapes the repository, uses a URL scheme, or is malformed.
 */
export function resolveRelativeRepoAsset(
  repoPath: string,
  ownerFilePath: string,
  rawRef: string,
): string | null {
  const trimmed = rawRef.trim();
  if (!repoPath || !isLocalAssetRef(trimmed)) return null;
  const withoutHash = trimmed.split("#", 1)[0];
  const withoutQuery = withoutHash.split("?", 1)[0];
  if (!withoutQuery) return null;

  let decoded: string;
  try {
    decoded = decodeURIComponent(withoutQuery);
  } catch {
    return null;
  }
  if (!decoded || decoded.includes("\0") || decoded.endsWith("/")) return null;

  const cleanOwnerPath = normalizeRepoRelativePath(ownerFilePath);
  const folder = cleanOwnerPath.includes("/")
    ? cleanOwnerPath.slice(0, cleanOwnerPath.lastIndexOf("/"))
    : "";
  const candidate = decoded.startsWith("/")
    ? decoded.slice(1)
    : folder
      ? `${folder}/${decoded}`
      : decoded;
  const resolved = resolveRepoFile(repoPath, { path: candidate, line: null, column: null });
  return resolved ? resolved.path : null;
}

export interface TerminalFileTarget {
  readonly path: string;
  readonly line: number | null;
  readonly column: number | null;
  readonly fallbackPath?: string;
}

/**
 * Resolves a hovered link, cursor token, or selected terminal token to a
 * repository-contained file target matching `matchesPath` for the terminal
 * viewport context menu.
 */
export function resolveTerminalFileCandidate(
  rawText: string | null | undefined,
  workPath: string,
  matchesPath: (path: string | null | undefined) => boolean,
  maxPathLength: number,
  activeCwdSubdir?: string | null,
): TerminalFileTarget | null {
  if (!rawText || !workPath) return null;
  const trimmed = rawText.trim();
  if (!trimmed || trimmed.length > maxPathLength || /[\r\n]/.test(trimmed)) return null;

  const tryResolve = (text: string): TerminalFileTarget | null => {
    const action = activeCwdSubdir
      ? resolveLinkAction(text, workPath, activeCwdSubdir)
      : resolveLinkAction(text, workPath);
    if (action.kind === "file" && matchesPath(action.path)) {
      return {
        path: action.path,
        line: action.line,
        column: action.column,
        fallbackPath: action.fallbackPath,
      };
    }
    if (activeCwdSubdir) {
      const rootAction = resolveLinkAction(text, workPath);
      if (rootAction.kind === "file" && matchesPath(rootAction.path)) {
        return {
          path: rootAction.path,
          line: rootAction.line,
          column: rootAction.column,
          fallbackPath: rootAction.fallbackPath,
        };
      }
    }
    return null;
  };

  const direct = tryResolve(trimmed);
  if (direct) return direct;

  // Also accept a single-token bare filename like `index.html` or `"logo.svg"`
  // which `resolveLinkAction` declines because bare filenames without `/` or `:line`
  // are not auto-underlined in prose.
  const stripped = trimmed.replace(/^["'`(<[{]+/, "").replace(/["'`)>\]}.,;:]+$/, "");
  if (!stripped || /\s/.test(stripped) || ASSET_SCHEME_RE.test(stripped) || !matchesPath(stripped)) {
    return null;
  }
  const cleanSubdir = activeCwdSubdir?.trim().replace(/\\/g, "/").replace(/^\/+|\/+$/g, "") ?? "";
  if (cleanSubdir && !stripped.startsWith("/") && !stripped.includes("\\")) {
    const subResolved = resolveRepoFile(workPath, {
      path: `${cleanSubdir}/${stripped}`,
      line: null,
      column: null,
    });
    const rootResolved = resolveRepoFile(workPath, { path: stripped, line: null, column: null });
    if (subResolved) {
      return {
        path: subResolved.path,
        line: null,
        column: null,
        fallbackPath:
          rootResolved && rootResolved.path !== subResolved.path ? rootResolved.path : undefined,
      };
    }
  }
  const resolved = resolveRepoFile(workPath, { path: stripped, line: null, column: null });
  if (!resolved || !matchesPath(resolved.path)) return null;
  return { path: resolved.path, line: null, column: null };
}
