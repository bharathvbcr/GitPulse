#!/usr/bin/env node
/**
 * Lint the GitHub Actions workflow files with actionlint.
 *
 * This exists because nothing else in the repository reads the workflow YAML.
 * `ci.yml` and `coverage.yml` run on every push, but `release.yml` runs only
 * on a `v*` tag — so a syntax error there stays invisible until a release is
 * cut, which is the worst moment to discover the file does not parse. A stray
 * indent on `permissions:` did exactly that once.
 *
 * actionlint is a separate binary rather than an npm dependency. A missing
 * binary exits 2 (checker could not run) and is reported differently from
 * workflows that were checked and found faulty, which exits 1.
 *
 * Three rules are read here without actionlint, because a release job holds a
 * write token: every third-party `uses:` is pinned to a commit SHA, every
 * checkout sets `persist-credentials: false`, and no step pipes a download
 * straight into a shell.
 *
 * Exit codes: 0 workflows are clean · 1 actionlint or a pinning rule found
 * problems · 2 the check could not run
 */

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const WORKFLOW_DIR = path.join(REPO_ROOT, ".github", "workflows");

// actionlint reads a handful of YAML files; a minute is generous. A kill at
// the deadline arrives as `result.error`, which `interpret` maps to exit 2.
export const ACTIONLINT_TIMEOUT_MS = 60_000;

export const INSTALL_HINT =
  "actionlint is not installed. Install it with `brew install actionlint`, or see " +
  "https://github.com/rhysd/actionlint/blob/main/docs/install.md";

/**
 * Workflow files actionlint is expected to cover, so an empty directory is
 * reported as a failure rather than silently passing with nothing checked.
 *
 * @param {string} dir
 * @returns {string[]}
 */
export function workflowFiles(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((name) => name.endsWith(".yml") || name.endsWith(".yaml"))
    .sort();
}

/**
 * True when `yaml` declares a workflow-level `permissions:` map (before `jobs:`).
 * Job-only permissions leave the rest of the file on GitHub's default token,
 * which is the hole `actions/missing-workflow-permissions` reports.
 *
 * @param {string} yaml
 * @returns {boolean}
 */
export function hasWorkflowLevelPermissions(yaml) {
  const jobsAt = yaml.search(/^jobs\s*:/m);
  const head = jobsAt === -1 ? yaml : yaml.slice(0, jobsAt);
  return /^permissions\s*:/m.test(head);
}

/**
 * Workflow filenames in `dir` that lack a top-level permissions block.
 *
 * @param {string} dir
 * @returns {string[]}
 */
export function workflowsMissingPermissions(dir) {
  return workflowFiles(dir).filter((name) => {
    const text = readFileSync(path.join(dir, name), "utf8");
    return !hasWorkflowLevelPermissions(text);
  });
}

/**
 * Every `uses:` ref that can be repointed under the workflow: a tag, a branch
 * or an abbreviated SHA. A local action (`./`) and a docker image have no such
 * ref. A tag is the hole: whoever controls the action's repository can move it,
 * and the next run executes that code with this job's token.
 *
 * @param {string} yaml
 * @returns {string[]}
 */
export function unpinnedActions(yaml) {
  return [...yaml.matchAll(/^\s*(?:- )?uses:\s+(\S+)/gm)]
    .map((match) => match[1])
    .filter((ref) => !ref.startsWith("./") && !ref.startsWith("docker://"))
    .filter((ref) => !/@[0-9a-f]{40}$/.test(ref));
}

/**
 * Indexes (0-based, in file order) of `actions/checkout` steps whose own
 * `with:` does not set `persist-credentials: false`. Without it the checkout
 * writes the job's token into `.git/config`, where every later step — and every
 * build script it runs — can read it.
 *
 * @param {string} yaml
 * @returns {number[]}
 */
export function checkoutsPersistingCredentials(yaml) {
  const steps = yaml.split(/\n(?=[ \t]*- )/).filter((step) => /^\s*(?:- )?uses:\s+actions\/checkout@/m.test(step));
  return steps.flatMap((step, index) => (/^\s+persist-credentials:\s*false\s*$/m.test(step) ? [] : [index]));
}

/**
 * @typedef {{ text: string, subs: string[] }} ShellWord  a word, and the source of each `$(...)`, `<(...)` or backtick inside it
 * @typedef {ShellWord[]} ShellStage  one command of a pipeline
 */

/**
 * Index just past the `)` that closes the substitution whose body starts at
 * `from`, skipping quoted text. An unterminated one runs to the end.
 *
 * @param {string} text
 * @param {number} from
 */
function closingParen(text, from) {
  let depth = 1;
  let index = from;
  while (index < text.length) {
    const ch = text[index];
    if (ch === "'" || ch === '"') {
      const close = text.indexOf(ch, index + 1);
      index = close === -1 ? text.length : close + 1;
      continue;
    }
    if (ch === "\\") index += 1;
    else if (ch === "(") depth += 1;
    else if (ch === ")" && --depth === 0) return index + 1;
    index += 1;
  }
  return text.length;
}

/**
 * Splits shell source into commands (at `;`, `&&`, `||`, `&`, newlines) and
 * each command into pipeline stages (at `|`), honouring quotes, backslashes and
 * comments, and keeping the body of every substitution for a second pass.
 * It is a classifier, not a shell: it never expands anything.
 *
 * @param {string} text
 * @returns {ShellStage[][]}
 */
function shellCommands(text) {
  /** @type {ShellStage[][]} */
  const commands = [];
  /** @type {ShellStage[]} */
  let stages = [];
  /** @type {ShellWord[]} */
  let words = [];
  /** @type {ShellWord | null} */
  let word = null;
  const endWord = () => { if (word) words.push(word); word = null; };
  const endStage = () => { endWord(); if (words.length > 0) stages.push(words); words = []; };
  const endCommand = () => { endStage(); if (stages.length > 0) commands.push(stages); stages = []; };
  const current = () => (word ??= { text: "", subs: [] });
  let index = 0;
  while (index < text.length) {
    const ch = text[index];
    const next = text[index + 1];
    if (ch === " " || ch === "\t" || ch === "\r") endWord();
    else if (ch === "#" && word === null) break;
    else if (ch === "\n" || ch === ";") endCommand();
    else if (ch === "&" && (text[index - 1] === ">" || text[index - 1] === "<" || next === ">")) current().text += ch;
    else if (ch === "&") { endCommand(); if (next === "&") index += 1; }
    else if (ch === "|" && next === "|") { endCommand(); index += 1; }
    else if (ch === "|") { endStage(); if (next === "&") index += 1; }
    else if (ch === "'") {
      const close = text.indexOf("'", index + 1);
      const end = close === -1 ? text.length : close;
      current().text += text.slice(index + 1, end);
      index = end;
    } else if (ch === '"') {
      const target = current();
      index += 1;
      while (index < text.length && text[index] !== '"') {
        if (text[index] === "\\") { target.text += text[index + 1] ?? ""; index += 2; continue; }
        if (text[index] === "$" && text[index + 1] === "(") {
          const end = closingParen(text, index + 2);
          target.subs.push(text.slice(index + 2, end - 1));
          index = end;
          continue;
        }
        if (text[index] === "`") {
          const close = text.indexOf("`", index + 1);
          const end = close === -1 ? text.length : close;
          target.subs.push(text.slice(index + 1, end));
          index = end + 1;
          continue;
        }
        target.text += text[index];
        index += 1;
      }
    } else if ((ch === "$" || ch === "<" || ch === ">") && next === "(") {
      const end = closingParen(text, index + 2);
      current().subs.push(text.slice(index + 2, end - 1));
      index = end - 1;
    } else if (ch === "`") {
      const close = text.indexOf("`", index + 1);
      const end = close === -1 ? text.length : close;
      current().subs.push(text.slice(index + 1, end));
      index = end;
    } else if (ch === "\\") {
      current().text += text[index + 1] ?? "";
      index += 1;
    } else current().text += ch;
    index += 1;
  }
  endCommand();
  return commands;
}

const SHELLS = new Set(["sh", "bash", "zsh", "dash", "ksh", "ash"]);
const SHELL_KEYWORDS = new Set(["if", "then", "else", "elif", "do", "while", "until", "!", "{", "(", "time", "exec", "command", "nohup", "env", "sudo", "doas"]);
/** Wrappers whose next word is an option's argument, not the command. */
const WRAPPER_OPTION_ARGUMENT = new Set(["-u", "-g", "-h", "-p", "-C", "-D", "-R", "-T", "-U", "-S"]);

/**
 * The word a stage runs, after assignments, wrappers (`sudo -E`, `env`, ...)
 * and control keywords. Its base name, and the words after it.
 *
 * @param {ShellStage} stage
 * @returns {{ name: string, args: ShellWord[] } | null}
 */
function stageCommand(stage) {
  let index = 0;
  while (index < stage.length) {
    const { text } = stage[index];
    if (/^[A-Za-z_]\w*=/.test(text)) { index += 1; continue; }
    if (SHELL_KEYWORDS.has(text)) {
      index += 1;
      if (text === "sudo" || text === "doas" || text === "env") {
        while (index < stage.length && stage[index].text.startsWith("-")) index += WRAPPER_OPTION_ARGUMENT.has(stage[index].text) && text !== "env" ? 2 : 1;
      }
      continue;
    }
    return { name: text.slice(text.lastIndexOf("/") + 1), args: stage.slice(index + 1) };
  }
  return null;
}

/** @param {ShellStage} stage */
function isDownloadStage(stage) {
  const command = stageCommand(stage);
  return command !== null && ["curl", "wget", "fetch"].includes(command.name);
}

/**
 * True when `source` has a command that downloads: `$(curl ...)` and
 * `<(wget ...)` bodies are searched the same way as the line itself.
 *
 * @param {string} source
 * @returns {boolean}
 */
function containsDownload(source) {
  return shellCommands(source).some((stages) => stages.some((stage) => isDownloadStage(stage) || stage.some((word) => word.subs.some(containsDownload))));
}

/**
 * A stage that executes text it is handed: a shell reading a script from stdin
 * or from a path, `source`, `.`, or `eval`.
 *
 * @param {ShellStage} stage
 * @returns {{ kind: "shell" | "eval", runsOwnText: boolean, args: ShellWord[] } | null}
 */
function stageExecutes(stage) {
  const command = stageCommand(stage);
  if (!command) return null;
  if (command.name === "eval") return { kind: "eval", runsOwnText: true, args: command.args };
  if (command.name === "source" || command.name === ".") return { kind: "shell", runsOwnText: false, args: command.args };
  if (!SHELLS.has(command.name)) return null;
  const dashC = command.args.some((arg) => /^-[A-Za-z]*c[A-Za-z]*$/.test(arg.text));
  return { kind: "shell", runsOwnText: dashC, args: command.args };
}

/**
 * Does this command run something it downloaded, without a check in between?
 * Either a download stage feeds a later stage that executes its stdin, or an
 * executing stage is handed a download through a substitution.
 *
 * @param {ShellStage[]} stages
 */
function runsDownload(stages) {
  return stages.some((stage, position) => {
    const runner = stageExecutes(stage);
    if (!runner) return false;
    if (runner.args.some((arg) => arg.subs.some(containsDownload))) return true;
    const readsStdin = position > 0 && runner.kind === "shell" && !runner.runsOwnText;
    return readsStdin && stages.slice(0, position).some((earlier) => isDownloadStage(earlier));
  });
}

/**
 * The shell lines of `yaml` as logical lines: a backslash continuation, and a
 * line that ends in `|`, `&&` or `||`, continue on the next line. A YAML block
 * header such as `run: |` ends in a pipe too and is not a continuation.
 *
 * @param {string} yaml
 * @returns {string[]}
 */
function logicalLines(yaml) {
  /** @type {string[]} */
  const logical = [];
  let pending = "";
  for (const raw of yaml.split(/\r?\n/)) {
    if (pending === "" && /^\s*#/.test(raw)) continue;
    const line = raw.replace(/\s+$/, "");
    if (/\\$/.test(line)) { pending += `${line.slice(0, -1)} `; continue; }
    const joined = pending + line;
    const blockHeader = /:\s*[|>][+-]?$/.test(joined);
    if (!blockHeader && /(?:\||&&)$/.test(joined)) { pending = `${joined} `; continue; }
    pending = "";
    logical.push(joined);
  }
  if (pending !== "") logical.push(pending);
  return logical;
}

/**
 * Logical lines that run a download without looking at it first:
 * `curl ... | bash`, `bash <(curl ...)`, `source <(...)`, `bash -c "$(curl ...)"`,
 * `eval "$(...)"`, through `sudo`, `/bin/bash` or a `tee`, however the line is
 * split across a continuation. A tag-addressed script is whatever its owner
 * last pushed under that name, so the job would execute unreviewed code with
 * its own token. Download to a file, check a pinned SHA-256, then run it.
 * Only a command in command position counts, so the word `curl` in an `echo`,
 * a comment or a step name does not, and `curl -o f URL; echo | sh -c x` is two
 * commands.
 *
 * @param {string} yaml
 * @returns {string[]}
 */
export function pipedDownloads(yaml) {
  return logicalLines(yaml)
    .map((line) => {
      const body = line.replace(/^\s*(?:-\s+)?run:\s*(?:[|>][+-]?\s*)?/, "");
      const quoted = /^(['"])(.*)\1$/.exec(body.trim());
      return quoted && !quoted[2].includes(quoted[1]) ? quoted[2] : body;
    })
    .filter((body) => shellCommands(body).some(runsDownload))
    .map((body) => body.trim());
}

/**
 * Every rule over every workflow in `dir`, one line per finding.
 *
 * @param {string} dir
 * @returns {string[]}
 */
export function workflowPolicyFindings(dir) {
  return workflowFiles(dir).flatMap((name) => {
    const text = readFileSync(path.join(dir, name), "utf8");
    return [
      ...pipedDownloads(text).map((line) => `${name}: runs a download without checking it (${line})`),
      ...unpinnedActions(text).map((ref) => `${name}: ${ref} is not pinned to a 40-character commit SHA`),
      ...checkoutsPersistingCredentials(text).map(
        (index) => `${name}: checkout #${index + 1} does not set persist-credentials: false`,
      ),
    ];
  });
}

/**
 * @param {{ status: number | null, error?: Error }} result
 * @returns {{ code: number, message: string | null }}
 */
export function interpret(result) {
  if (result.error && /** @type {NodeJS.ErrnoException} */ (result.error).code === "ENOENT") {
    return { code: 2, message: INSTALL_HINT };
  }
  if (result.error) return { code: 2, message: `actionlint could not run: ${result.error.message}` };
  if (result.status === 0) return { code: 0, message: null };
  if (result.status === 1) return { code: 1, message: "actionlint reported problems" };
  return { code: 2, message: `actionlint exited with ${result.status}` };
}

export function main() {
  const files = workflowFiles(WORKFLOW_DIR);
  if (files.length === 0) {
    console.error(`FAIL: no workflow files found in ${WORKFLOW_DIR} — nothing was checked`);
    return 2;
  }
  // Read before actionlint and reported whatever it says: these rules need no
  // binary, so a missing actionlint must not hide them.
  const findings = workflowPolicyFindings(WORKFLOW_DIR);
  for (const finding of findings) console.error(`FAIL: ${finding}`);
  const result = spawnSync("actionlint", ["-color"], {
    cwd: REPO_ROOT,
    stdio: "inherit",
    timeout: ACTIONLINT_TIMEOUT_MS,
    killSignal: "SIGKILL",
  });
  const { code, message } = interpret(result);
  if (code !== 0) console.error(`FAIL: ${message}`);
  // A rule that was checked and broken outranks an actionlint that could not run.
  if (findings.length) return 1;
  if (code === 0) {
    console.log(`OK: ${files.length} workflow files lint clean, every action pinned to a commit, no checkout keeps its token, no download is piped into a shell (${files.join(", ")})`);
  }
  return code;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
