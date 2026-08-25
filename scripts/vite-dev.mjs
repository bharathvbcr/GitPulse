import {
  defaultRepoRoot,
  formatResolveMessage,
  isTauriHookEnv,
  parseOptionalPort,
  resolveDevPort,
  spawnLocalBin,
} from "./dev-port.mjs";

const repoRoot = defaultRepoRoot();
const envLocked = parseOptionalPort(process.env.GITPULSE_DEV_PORT) != null;
const result = await resolveDevPort({
  repoRoot,
  allowAutoport: !envLocked && !isTauriHookEnv(),
});
process.env.GITPULSE_DEV_PORT = String(result.port);

const notice = formatResolveMessage(result);
if (notice) console.info(notice);

spawnLocalBin(repoRoot, "vite", process.argv.slice(2));
