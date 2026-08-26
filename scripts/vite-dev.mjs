import {
  defaultRepoRoot,
  formatResolveMessage,
  isTauriHookEnv,
  parseOptionalPort,
  resolveDevPort,
  spawnLocalBin,
} from "./dev-port.mjs";

const repoRoot = defaultRepoRoot();
let envLocked;
try {
  envLocked = parseOptionalPort(process.env.GITPULSE_DEV_PORT) != null;
} catch (err) {
  // A garbage GITPULSE_DEV_PORT must fail with a readable message and a
  // nonzero exit, not a raw unhandled rejection.
  console.error(`vite-dev: ${err.message}`);
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
