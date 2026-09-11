/**
 * Documented one-liners for DevCouncil components.
 *
 * Native Go/Rust binaries only — there is no uv / Python install path.
 * The in-app ladder still installs `devmap` alone; this module is the
 * bulk / selective path the setup wizard copies or runs in Console.
 */

export const PUBLIC_DEVCOUNCIL_GIT = "https://github.com/bharathvbcr/DevCouncil.git";

/** Wall-clock budget matching the in-app install deadline (20 minutes). */
export const INSTALL_TIMEOUT_SECS = 1200;

export type DevcouncilPreset = "devmap" | "analysis" | "all";

export const DEVCOUNCIL_PRESETS: {
  id: DevcouncilPreset;
  label: string;
  detail: string;
}[] = [
  {
    id: "devmap",
    label: "DevMap only",
    detail: "Standalone code-intelligence CLI. GitPulse Map needs only this.",
  },
  {
    id: "analysis",
    label: "Analysis suite",
    detail: "devmap, dcstore, dcverify, dcgrep — no Go host.",
  },
  {
    id: "all",
    label: "Full DevCouncil",
    detail: "Go host (devcouncil / dev) plus the analysis suite.",
  },
];

export interface InstallCommandInput {
  preset: DevcouncilPreset;
  /** Absolute DevCouncil checkout, when GitPulse has detected one. */
  sourceCheckout: string | null;
  windows?: boolean;
}

export interface InstallCommand {
  command: string;
  label: string;
  timeoutSecs: number;
  /** True when the command is a single argv-runnable line (Console can run it). */
  runnable: boolean;
}

function quoteUnix(path: string): string {
  return `'${path.replace(/'/g, `'\\''`)}'`;
}

function quoteWin(path: string): string {
  return `'${path.replace(/'/g, "''")}'`;
}

function joinUnix(root: string, ...parts: string[]): string {
  return [root.replace(/\/+$/, ""), ...parts].join("/");
}

function joinWin(root: string, ...parts: string[]): string {
  return [root.replace(/[\\/]+$/, ""), ...parts].join("\\");
}

function unsafePath(path: string): boolean {
  return /[\0\n\r]/.test(path);
}

const REMOTE_DEVMAP = `cargo install --git ${PUBLIC_DEVCOUNCIL_GIT} --locked --force devmap-cli`;
const REMOTE_DCSTORE = `cargo install --git ${PUBLIC_DEVCOUNCIL_GIT} --locked --force --bin dcstore dc-store`;
const REMOTE_DCVERIFY = `cargo install --git ${PUBLIC_DEVCOUNCIL_GIT} --locked --force --bin dcverify dc-verify`;
const REMOTE_DCGREP = `cargo install --git ${PUBLIC_DEVCOUNCIL_GIT} --locked --force --bin dcgrep dc-grep`;
const REMOTE_HOST =
  "go install github.com/bharathvbcr/DevCouncil/backend/go_orchestrator/cmd/devcouncil@latest";

function remoteChain(preset: DevcouncilPreset): string {
  const rust = [REMOTE_DEVMAP, REMOTE_DCSTORE, REMOTE_DCVERIFY, REMOTE_DCGREP];
  if (preset === "devmap") return REMOTE_DEVMAP;
  if (preset === "analysis") return rust.join(" && ");
  return [REMOTE_HOST, ...rust].join(" && ");
}

export function buildDevcouncilInstallCommand(input: InstallCommandInput): InstallCommand {
  const windows = input.windows ?? false;
  const timeoutSecs = INSTALL_TIMEOUT_SECS;
  const label =
    input.preset === "devmap"
      ? "Install DevMap"
      : input.preset === "analysis"
        ? "Install analysis suite"
        : "Install DevCouncil";

  const checkout = input.sourceCheckout?.trim() || null;
  if (checkout && unsafePath(checkout)) {
    return { command: "", label, timeoutSecs, runnable: false };
  }
  if (checkout) {
    if (windows) {
      const script = joinWin(checkout, "scripts", "install.ps1");
      const components =
        input.preset === "devmap"
          ? "devmap"
          : input.preset === "analysis"
            ? "analysis"
            : "all";
      return {
        command: `powershell -NoProfile -File ${quoteWin(script)} -Components ${components}`,
        label,
        timeoutSecs,
        runnable: true,
      };
    }
    if (input.preset === "all") {
      return {
        command: `bash ${quoteUnix(joinUnix(checkout, "scripts", "install.sh"))}`,
        label,
        timeoutSecs,
        runnable: true,
      };
    }
    if (input.preset === "analysis") {
      return {
        command: `bash ${quoteUnix(joinUnix(checkout, "scripts", "install-components.sh"))} analysis`,
        label,
        timeoutSecs,
        runnable: true,
      };
    }
    return {
      command: `bash ${quoteUnix(joinUnix(checkout, "scripts", "install-components.sh"))} devmap`,
      label,
      timeoutSecs,
      runnable: true,
    };
  }

  const chain = remoteChain(input.preset);
  if (windows) {
    return {
      command: `powershell -NoProfile -Command ${quoteWin(chain.replace(/ && /g, "; "))}`,
      label,
      timeoutSecs,
      runnable: true,
    };
  }
  if (input.preset === "devmap") {
    return { command: chain, label, timeoutSecs, runnable: true };
  }
  return {
    command: `bash -c ${quoteUnix(chain)}`,
    label,
    timeoutSecs,
    runnable: true,
  };
}
