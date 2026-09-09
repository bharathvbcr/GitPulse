/** Convert a Git transport URL to a browser URL without credentials or transport-only options. */
export function remoteWebsite(raw: string | null): string | null {
  if (!raw || raw.length > 16384 || /[\s\u0000-\u001f\u007f]/.test(raw)) return null;
  let value = raw;
  if (!value.includes("://")) {
    const scp = /^(?:[^/@:]+@)?([^/:]+):([^/].*)$/.exec(value);
    if (!scp || scp[1].length === 1) return null;
    // A bare protocol-like prefix is not enough to infer a web host. SSH
    // aliases without a user or DNS name need an explicit website URL.
    if (!value.includes("@") && !scp[1].includes(".") && scp[1] !== "localhost") return null;
    value = `ssh://${scp[1]}/${scp[2]}`;
  }
  try {
    const url = new URL(value);
    if (!["https:", "http:", "ssh:", "git:"].includes(url.protocol) || !url.hostname || !url.pathname.replace(/\//g, "")) return null;
    if (url.protocol === "ssh:" || url.protocol === "git:") {
      // WHATWG URLs cannot switch a non-special scheme to a special scheme in place.
      const website = new URL(`https://${url.hostname}${url.pathname}`);
      website.pathname = website.pathname.replace(/\.git\/?$/, "");
      return website.href;
    }
    url.username = "";
    url.password = "";
    url.search = "";
    url.hash = "";
    url.pathname = url.pathname.replace(/\.git\/?$/, "");
    return url.href;
  } catch { return null; }
}
