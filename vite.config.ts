import { defineConfig, type Plugin } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { isTauriHookEnv, portFromEnv } from "./scripts/dev-port.mjs";
import { appVersion } from "./scripts/app-version.mjs";

/**
 * Entry-chunk ceiling. Two things are kept out of the entry chunk, so this
 * bounds the app shell plus the eager `work` views and nothing else:
 *   - vendor runtimes (svelte, xterm, lucide, tauri) via `gitpulseManualChunk`
 *   - every `inspect`/`more` view, via the dynamic imports in
 *     lib/views/viewLoaders.ts
 *
 * Measured at 558 KB with that split in place. The trajectory before it, when
 * every view was statically imported, was 648 → 656 → 680 → 692 → 708 → 720 →
 * 754 KB, reaching 813 KB once the Pulse view landed — which is what forced
 * the split rather than another raise. Splitting the inspect/more groups
 * returned 249 KB, so a view-shaped addition now costs nothing here.
 *
 * Before raising this, run the two checks that tell you what actually grew:
 *   - vendor chunks unchanged ⇒ the growth is first-party app code
 *   - vendor chunks changed  ⇒ a dependency leaked into the entry chunk
 * If the growth is a new view, split it in viewLoaders.ts instead — the
 * contract test in lib/views/viewLoaders.test.ts already requires that for
 * anything outside the `work` group.
 */
const MAX_PRODUCTION_CHUNK_BYTES = 780_000;

/**
 * Keep independently cacheable runtimes out of the application entry chunk.
 * Vite normalizes module ids, but replacing separators keeps this deterministic
 * when the same configuration is exercised directly on Windows.
 */
export function gitpulseManualChunk(id: string): string | undefined {
  const normalized = id.replaceAll("\\", "/");
  if (!normalized.includes("/node_modules/")) return undefined;
  if (normalized.includes("/node_modules/@xterm/")) return "vendor-xterm";
  if (normalized.includes("/node_modules/lucide-svelte/")) return "vendor-icons";
  if (normalized.includes("/node_modules/svelte/")) return "vendor-svelte";
  if (normalized.includes("/node_modules/@tauri-apps/")) return "vendor-tauri";
  return "vendor";
}

/** Fail the build if later imports silently recreate the monolithic bundle. */
function gitpulseBundleBudget(): Plugin {
  return {
    name: "gitpulse-bundle-budget",
    apply: "build",
    generateBundle(_options, bundle) {
      for (const output of Object.values(bundle)) {
        if (output.type !== "chunk") continue;
        const bytes = Buffer.byteLength(output.code, "utf8");
        if (bytes > MAX_PRODUCTION_CHUNK_BYTES) {
          this.error(
            `production chunk ${output.fileName} is ${bytes} bytes; ` +
              `the GitPulse limit is ${MAX_PRODUCTION_CHUNK_BYTES} bytes`,
          );
        }
      }
    },
  };
}

/**
 * WKWebView cannot apply Vite's ESM/CSS hot swap (`module.default` is
 * undefined, then the web content process dies). A full reload is slower
 * than HMR but leaves the webview in a known-good state; in-flight Tauri
 * IPC callbacks still warn, and diagnostics drops that host chatter.
 *
 * Decision is a pure function so tests cover the Tauri/browser split without
 * constructing a Vite plugin context. `modules: []` suppresses ESM HMR;
 * `undefined` leaves Vite's default HMR path alone.
 */
export function tauriHotUpdateDecision(
  env: NodeJS.ProcessEnv,
  environmentName: string,
): { reloadClient: boolean; modules: [] | undefined } {
  if (!isTauriHookEnv(env)) {
    return { reloadClient: false, modules: undefined };
  }
  return { reloadClient: environmentName === "client", modules: [] };
}

/**
 * Intercept HMR *before* vite-plugin-svelte. The Svelte hook compiles and can
 * throw; Vite then skips later plugins — which is how a failed PromptModal
 * reload used to leave App.svelte referencing an undeclared binding.
 */
export function gitpulseTauriFullReload(
  env: NodeJS.ProcessEnv = process.env,
): Plugin {
  return {
    name: "gitpulse-tauri-full-reload",
    hotUpdate: {
      order: "pre",
      handler() {
        const decision = tauriHotUpdateDecision(env, this.environment.name);
        if (decision.reloadClient) {
          this.environment.hot.send({ type: "full-reload" });
        }
        return decision.modules;
      },
    },
  };
}

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [
    gitpulseTauriFullReload(),
    svelte({
      compilerOptions: {
        // Accept handlers crash WKWebView on `module.default`. Off under Tauri
        // so a leaked HMR payload still falls back to a full reload.
        hmr: !isTauriHookEnv(),
      },
    }),
    gitpulseBundleBudget(),
  ],
  clearScreen: false,
  // Stamped into diagnostics entries so a log copied after an upgrade says
  // which build actually recorded each line.
  define: {
    __APP_VERSION__: JSON.stringify(appVersion()),
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks: gitpulseManualChunk,
      },
    },
  },
  server: {
    port: portFromEnv(),
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
});
