import {
  defaultRepoRoot,
  formatResolveMessage,
  isTauriHookEnv,
  parseOptionalPort,
  resolveDevPort,
  spawnLocalBin,
} from "./dev-port.mjs";
import { wantsHelp } from "./usage.mjs";

const repoRoot = defaultRepoRoot();

// `npm run dev -- --help` wants vite's usage, not a dev server. Resolving a
// port first would print a port notice and could reclaim a held port as a side
// effect of asking a question.
if (wantsHelp(process.argv.slice(2))) {
  spawnLocalBin(repoRoot, "vite", process.argv.slice(2));
} else {
  let envLocked;
  try {
    envLocked = parseOptionalPort(process.env.GITPULSE_DEV_PORT) != null;
  } catch (err) {
    // A garbage GITPULSE_DEV_PORT must fail with a readable message and a
    // nonzero exit, not a raw unhandled rejection.
    const message = err instanceof Error ? err.message : String(err);
    console.error(`vite-dev: ${message}`);
    process.exit(2);
  }
  const result = await resolveDevPort({
    repoRoot,
    allowAutoport: !envLocked && !isTauriHookEnv(),
  });
  process.env.GITPULSE_DEV_PORT = String(result.port);

  const notice = formatResolveMessage(result);
  if (notice) console.info(notice);

  spawnLocalBin(repoRoot, "vite", process.argv.slice(2));
}

