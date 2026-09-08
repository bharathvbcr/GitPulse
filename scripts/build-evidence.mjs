import { createHash } from "node:crypto";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";

/** Keep symbolication evidence private; hidden source maps alone still ship.
 * @param {ReturnType<typeof import('./app-version.mjs').appBuild>} stamp
 * @param {string} [root]
 * @returns {import('vite').Plugin}
 */
export function privateSourceMaps(stamp, root = path.resolve(".build-evidence")) {
  const directory = path.resolve(root, stamp.id);
  return {
    name: "gitpulse-private-source-maps",
    apply: "build",
    enforce: "post",
    generateBundle: {
      order: "post",
      handler(_options, bundle) {
        mkdirSync(directory, { recursive: true, mode: 0o700 });
        /** @type {Record<string, string>} */
        const chunks = {};
        let maps = 0;
        for (const [name, output] of Object.entries(bundle)) {
          const destination = path.resolve(directory, name);
          if (!destination.startsWith(`${directory}${path.sep}`)) this.error("Unsafe build evidence path");
          if (output.type === "chunk" || name.endsWith(".map")) {
            const content = output.type === "chunk" ? output.code : output.source;
            mkdirSync(path.dirname(destination), { recursive: true, mode: 0o700 });
            writeFileSync(destination, content, { mode: 0o600 });
            if (output.type === "chunk") chunks[name] = createHash("sha256").update(output.code).digest("hex");
            if (name.endsWith(".map")) { maps++; delete bundle[name]; }
          }
        }
        if (!maps) this.error("Build produced no private source maps");
        writeFileSync(path.join(directory, "manifest.json"), JSON.stringify({ ...stamp, chunks, maps }, null, 2), { mode: 0o600 });
        this.emitFile({ type: "asset", fileName: "build-info.json", source: JSON.stringify(stamp) });
      },
    },
  };
}
