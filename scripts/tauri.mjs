import path from "node:path";
import {
  defaultRepoRoot,
  formatResolveMessage,
  isTauriDevArgs,
  resolveDevPort,
  spawnLocalBin,
  withTauriDevUrl,
} from "./dev-port.mjs";

const repoRoot = defaultRepoRoot();
const args = process.argv.slice(2);
const env = { ...process.env };

if (process.platform === "darwin") {
  const binDir = path.join(repoRoot, "scripts", "bin");
  env.PATH = env.PATH ? `${binDir}:${env.PATH}` : binDir;
}

if (isTauriDevArgs(args)) {
  const result = await resolveDevPort({ repoRoot, env, allowAutoport: true });
  env.GITPULSE_DEV_PORT = String(result.port);
  const notice = formatResolveMessage(result);
  if (notice) console.info(notice);
  args.splice(0, args.length, ...withTauriDevUrl(args, result.port));
}

spawnLocalBin(repoRoot, "tauri", args, env);
