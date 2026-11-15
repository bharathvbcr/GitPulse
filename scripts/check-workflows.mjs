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
 * A fourth rule guards the release gate, which reads a workflow run's
 * conclusion and never its jobs: a trusted job or step that can be skipped, or
 * can fail without failing, would let the gate pass on a run where the check
 * never ran (see "Release-gate rule" below).
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
const RELEASE_STATE_PATH = path.join(REPO_ROOT, "scripts", "release-state.mjs");

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

// ---------------------------------------------------------------------------
// Release-gate rule: a check the gate trusts must not be able to skip.
//
// `release-state.mjs` and `.githooks/pre-push` read the newest push run of each
// trusted workflow and accept `completed` + `success`. They never read the
// run's jobs, and GitHub concludes a run `success` when a job was skipped (a
// false job-level `if:`, or a `needs:` on a skipped job) or when a step that
// failed was marked `continue-on-error`. So a check that gained one of those
// would satisfy the gate on a run in which it never ran — a check that did not
// run reading exactly like one that passed.
//
// The workflow YAML is read with a small block-YAML reader below rather than
// regexes, because the rule has to tell a step's own `if:` from the text
// `if:` inside a `run:` block, a `with:` input, a comment or a string. The
// reader covers the subset workflows use and throws on anything else
// (anchors, aliases, tags, merge keys, tab indentation, duplicate keys): a
// file it cannot read is a failure, never a pass.
// ---------------------------------------------------------------------------

/**
 * @typedef {{ kind: "scalar", value: string, quoted: boolean }} YamlScalar
 * @typedef {{ kind: "flow", text: string, items: string[] | null }} YamlFlow  `[a, b]` or `{k: v}`; `items` is set for a flat sequence
 * @typedef {YamlScalar | YamlFlow | Map<string, YamlNode> | YamlNode[]} YamlNode
 * @typedef {{ indent: number, body: string, number: number, blank: boolean, comment: boolean, tabIndented: boolean }} YamlLine
 */

class YamlError extends Error {}

/** @param {string} value @param {boolean} quoted @returns {YamlScalar} */
const scalar = (value, quoted) => ({ kind: "scalar", value, quoted });

/**
 * The quoted scalar that starts at `from`, or null if it is not closed in `text`.
 *
 * @param {string} text
 * @param {number} from
 * @returns {{ value: string, end: number } | null}
 */
function readQuoted(text, from) {
  const quote = text[from];
  let out = "";
  for (let i = from + 1; i < text.length; i += 1) {
    const ch = text[i];
    if (quote === "'") {
      if (ch !== "'") { out += ch; continue; }
      if (text[i + 1] === "'") { out += "'"; i += 1; continue; }
      return { value: out, end: i + 1 };
    }
    if (ch === '"') return { value: out, end: i + 1 };
    if (ch !== "\\") { out += ch; continue; }
    const next = text[i + 1] ?? "";
    i += 1;
    const simple = /** @type {Record<string, string>} */ ({ n: "\n", t: "\t", r: "\r", 0: "\0", '"': '"', "\\": "\\", "/": "/", " ": " " });
    if (next in simple) out += simple[next];
    else if (next === "x" || next === "u") {
      const width = next === "x" ? 2 : 4;
      const hex = text.slice(i + 1, i + 1 + width);
      if (!new RegExp(`^[0-9a-fA-F]{${width}}$`).test(hex)) throw new YamlError("invalid escape in a double-quoted string");
      out += String.fromCharCode(Number.parseInt(hex, 16));
      i += width;
    } else out += `\\${next}`;
  }
  return null;
}

/**
 * Text of a flow collection that opens `text`, once its brackets balance, and
 * where it ends. Null while it is still open.
 *
 * @param {string} text
 * @returns {number | null} index just past the closing bracket
 */
function flowEnd(text) {
  let depth = 0;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (ch === "'" || ch === '"') {
      const quoted = readQuoted(text, i);
      if (!quoted) return null;
      i = quoted.end - 1;
    } else if (ch === "[" || ch === "{") depth += 1;
    else if ((ch === "]" || ch === "}") && --depth === 0) return i + 1;
  }
  return null;
}

/**
 * Items of a flat `[a, 'b', "c"]`; null when it nests or is a `{...}` mapping.
 *
 * @param {string} text
 * @returns {string[] | null}
 */
function flatFlowItems(text) {
  if (!text.startsWith("[")) return null;
  const inner = text.slice(1, -1);
  /** @type {string[]} */
  const items = [];
  let current = "";
  let sawQuoted = false;
  const push = () => {
    const trimmed = current.trim();
    if (trimmed !== "" || sawQuoted) items.push(trimmed);
    current = "";
    sawQuoted = false;
  };
  for (let i = 0; i < inner.length; i += 1) {
    const ch = inner[i];
    if (ch === "'" || ch === '"') {
      const quoted = readQuoted(inner, i);
      if (!quoted || current.trim() !== "") return null;
      current = quoted.value;
      sawQuoted = true;
      i = quoted.end - 1;
    } else if (ch === "[" || ch === "]" || ch === "{" || ch === "}") return null;
    else if (ch === ",") push();
    else current += ch;
  }
  push();
  return items;
}

/**
 * Splits `key: rest`. A plain key ends at the first `:` followed by a blank or
 * the end of the line; a quoted key is read as a string. Null when the text is
 * not a mapping entry (a scalar, a flow collection, a sequence item).
 *
 * @param {string} body
 * @returns {{ key: string, rest: string } | null}
 */
function splitKey(body) {
  if (body.startsWith("[") || body.startsWith("{") || body.startsWith("#") || body.startsWith("|") || body.startsWith(">")) return null;
  if (body.startsWith('"') || body.startsWith("'")) {
    const quoted = readQuoted(body, 0);
    if (!quoted) return null;
    const after = /^[ \t]*:(?:[ \t]+(.*)|)$/.exec(body.slice(quoted.end));
    return after ? { key: quoted.value, rest: (after[1] ?? "").trim() } : null;
  }
  const match = /:(?:[ \t]|$)/.exec(body);
  if (!match || match.index === 0) return null;
  const key = body.slice(0, match.index).trimEnd();
  if (/[ \t]#/.test(key)) return null;
  return { key, rest: body.slice(match.index + 1).trim() };
}

/** @param {string} body */
const isSequenceItem = (body) => body === "-" || /^-[ \t]/.test(body);

/** @param {string} text @returns {YamlLine[]} */
function yamlLines(text) {
  return text
    .replace(/^﻿/, "")
    .split(/\r\n|\n|\r/)
    .map((raw, index) => {
      const indent = /^ */.exec(raw)?.[0].length ?? 0;
      const body = raw.slice(indent).replace(/[ \t]+$/, "");
      return {
        indent,
        body,
        number: index + 1,
        blank: body === "",
        comment: body.startsWith("#"),
        tabIndented: body.startsWith("\t"),
      };
    });
}

/**
 * Reads the block-YAML subset GitHub workflows use. Throws a YamlError for
 * anything outside it rather than guessing.
 *
 * @param {string} text
 * @returns {YamlNode}
 */
export function parseWorkflowYaml(text) {
  const lines = yamlLines(text);
  let pos = 0;
  /** @param {YamlLine} line @param {string} message */
  const fail = (line, message) => new YamlError(`line ${line.number}: ${message}`);
  const skipInsignificant = () => {
    while (pos < lines.length && (lines[pos].blank || lines[pos].comment)) pos += 1;
  };
  /** @returns {YamlLine | null} */
  const peek = () => {
    skipInsignificant();
    return pos < lines.length ? lines[pos] : null;
  };
  /** @param {YamlLine} line */
  const requireSpaceIndent = (line) => {
    if (line.tabIndented) throw fail(line, "a tab is used as indentation");
  };

  /**
   * The value after `key:` or `- `, with `pos` already past the line that holds
   * it. `indent` is the column of the key (or dash) that owns the value.
   *
   * @param {string} rest
   * @param {number} indent
   * @param {YamlLine} line
   * @returns {YamlNode}
   */
  function valueFrom(rest, indent, line) {
    if (rest === "" || rest.startsWith("#")) {
      const next = peek();
      if (next && next.indent > indent) return parseNode(indent + 1);
      if (next && next.indent === indent && isSequenceItem(next.body)) return parseSequence(indent);
      return scalar("", false);
    }
    if (rest.startsWith("|") || rest.startsWith(">")) {
      if (!/^[|>](?:[+-]?\d?|\d?[+-]?)[ \t]*(?:#.*)?$/.test(rest)) throw fail(line, "unsupported block scalar header");
      /** @type {string[]} */
      const content = [];
      while (pos < lines.length && (lines[pos].blank || lines[pos].indent > indent)) {
        content.push(lines[pos].body);
        pos += 1;
      }
      return scalar(content.join("\n"), true);
    }
    if (rest.startsWith("&") || rest.startsWith("*") || rest.startsWith("!!") || rest.startsWith("!<") || rest.startsWith("%") || rest.startsWith("@") || rest.startsWith("`")) {
      throw fail(line, `unsupported YAML construct "${rest.slice(0, 12)}" (anchor, alias, tag or reserved indicator)`);
    }
    if (rest.startsWith("[") || rest.startsWith("{")) {
      let acc = rest;
      let end = flowEnd(acc);
      while (end === null) {
        if (pos >= lines.length) throw fail(line, "a flow collection is never closed");
        acc += ` ${lines[pos].body}`;
        pos += 1;
        end = flowEnd(acc);
      }
      const tail = acc.slice(end).trim();
      if (tail !== "" && !tail.startsWith("#")) throw fail(line, "text follows a flow collection");
      const flow = acc.slice(0, end);
      return { kind: "flow", text: flow, items: flatFlowItems(flow) };
    }
    if (rest.startsWith('"') || rest.startsWith("'")) {
      let acc = rest;
      let quoted = readQuoted(acc, 0);
      while (!quoted) {
        if (pos >= lines.length) throw fail(line, "a quoted string is never closed");
        acc += ` ${lines[pos].body}`;
        pos += 1;
        quoted = readQuoted(acc, 0);
      }
      const tail = acc.slice(quoted.end).trim();
      if (tail !== "" && !tail.startsWith("#")) throw fail(line, "text follows a quoted string");
      return scalar(quoted.value, true);
    }
    // A plain scalar ends at " #"; deeper-indented lines continue it.
    /** @param {string} s */
    const stripComment = (s) => s.replace(/[ \t]+#.*$/, "").trim();
    let value = stripComment(rest);
    for (;;) {
      let look = pos;
      while (look < lines.length && lines[look].blank) look += 1;
      if (look >= lines.length || lines[look].comment || lines[look].indent <= indent) break;
      value += ` ${stripComment(lines[look].body)}`;
      pos = look + 1;
    }
    return scalar(value.trim(), false);
  }

  /** @param {number} indent @returns {YamlNode} */
  function parseSequence(indent) {
    /** @type {YamlNode[]} */
    const items = [];
    for (;;) {
      const line = peek();
      if (!line || line.indent < indent) break;
      if (line.indent > indent) throw fail(line, "unexpected indentation in a sequence");
      if (!isSequenceItem(line.body)) break;
      requireSpaceIndent(line);
      const match = /^-([ \t]*)(.*)$/.exec(line.body);
      const gap = match?.[1].length ?? 0;
      const after = match?.[2] ?? "";
      if (after === "" || after.startsWith("#")) {
        pos += 1;
        const next = peek();
        items.push(next && next.indent > indent ? parseNode(indent + 1) : scalar("", false));
      } else if (splitKey(after) || isSequenceItem(after)) {
        // The item's own lines start at the column after the dash.
        lines[pos] = { ...line, indent: indent + 1 + gap, body: after };
        items.push(parseNode(indent + 1 + gap));
      } else {
        pos += 1;
        items.push(valueFrom(after, indent, line));
      }
    }
    return items;
  }

  /** @param {number} indent @returns {YamlNode} */
  function parseMapping(indent) {
    /** @type {Map<string, YamlNode>} */
    const map = new Map();
    for (;;) {
      const line = peek();
      if (!line || line.indent < indent) break;
      if (line.indent > indent) throw fail(line, "unexpected indentation in a mapping");
      requireSpaceIndent(line);
      if (isSequenceItem(line.body)) throw fail(line, "a sequence item where a mapping key was expected");
      if (line.body.startsWith("? ")) throw fail(line, "a complex mapping key is not supported");
      const entry = splitKey(line.body);
      if (!entry) throw fail(line, "expected `key: value`");
      if (entry.key === "<<") throw fail(line, "a merge key is not supported");
      if (map.has(entry.key)) throw fail(line, `duplicate key "${entry.key}"`);
      pos += 1;
      map.set(entry.key, valueFrom(entry.rest, indent, line));
    }
    return map;
  }

  /** @param {number} minIndent @returns {YamlNode} */
  function parseNode(minIndent) {
    const line = peek();
    if (!line || line.indent < minIndent) return scalar("", false);
    requireSpaceIndent(line);
    if (isSequenceItem(line.body)) return parseSequence(line.indent);
    if (splitKey(line.body)) return parseMapping(line.indent);
    pos += 1;
    return valueFrom(line.body, line.indent - 1, line);
  }

  if (peek()?.body === "---") pos += 1;
  const root = parseNode(0);
  const rest = peek();
  if (rest) throw fail(rest, "unexpected content after the document");
  return root;
}

/**
 * One entry of the release-gate allowlist. `key` is the rule being waived:
 * `if` (a job- or step-level condition), `continue-on-error`, `needs` (a job
 * that needs a job which can skip) or `matrix.exclude`. `step` is the step's
 * `name`, or `#<index>` (0-based), and is absent for a job-level waiver.
 *
 * @typedef {{ workflow: string, job: string, step?: string, key: string, reason: string }} ReleaseGateAllowEntry
 */

/**
 * Every exemption from the release-gate rule, each with the reason it is safe.
 * An entry that no longer matches a condition in the workflows fails the check,
 * so this list cannot outlive what it excuses.
 *
 * @type {ReleaseGateAllowEntry[]}
 */
export const RELEASE_GATE_ALLOWLIST = [
  {
    workflow: "ci.yml",
    job: "arm-bundle",
    key: "if",
    reason:
      "Dispatch-only ARM release-matrix build (each leg is a full release build). The release gate reads only `event=push` runs, where this job is skipped by design. It is NOT a check the gate vouches for: a green push run says nothing about ARM bundling.",
  },
  {
    workflow: "ci.yml",
    job: "arm-bundle",
    step: "Install System Dependencies (Linux)",
    key: "if",
    reason:
      "Selects the Linux leg of the two-leg ARM matrix with startsWith(matrix.platform, 'ubuntu-'), a platform selector in a different spelling from the `==` form the rule accepts. The job itself is dispatch-only.",
  },
];

const ALLOW_KEYS = new Set(["if", "continue-on-error", "needs", "matrix.exclude"]);
const SKIP_HINT = "remove the condition or add an allowlist entry with a reason (RELEASE_GATE_ALLOWLIST in scripts/check-workflows.mjs)";

/**
 * Workflow files the release gate reads, taken from the call sites of
 * `requireSuccessfulPushRun` in `release-state.mjs` so this rule cannot drift
 * from the gate. Every mention of the function must be the definition or a call
 * whose workflow names are string literals; any other shape throws, because a
 * call site that cannot be read is a gate this rule cannot follow.
 *
 * @param {string} source  text of release-state.mjs
 * @returns {{ workflows: string[], sites: string[][] }}
 */
export function trustedWorkflowNames(source) {
  const code = source
    .split(/\r?\n/)
    .filter((line) => !/^\s*(?:\/\/|\/\*|\*)/.test(line))
    .join("\n");
  const mentions = (code.match(/\brequireSuccessfulPushRun\b/g) ?? []).length;
  const definitions = (code.match(/\bfunction\s+requireSuccessfulPushRun\b/g) ?? []).length;
  /** @type {string[][]} */
  const sites = [];
  /** @param {string} name */
  const checked = (name) => {
    if (!/^[A-Za-z0-9_.-]+\.ya?ml$/.test(name)) throw new Error(`"${name}" is not a plain workflow file name`);
    return name;
  };
  for (const match of code.matchAll(/for\s*\(\s*const\s+([A-Za-z_$][\w$]*)\s+of\s+\[([^\]]*)\]\s*\)\s*requireSuccessfulPushRun\(\s*\1\s*\)/g)) {
    sites.push(
      match[2].split(",").map((item) => {
        const literal = /^\s*(["'])([^"']+)\1\s*$/.exec(item);
        if (!literal) throw new Error(`a requireSuccessfulPushRun call site lists something other than string literals (${item.trim()})`);
        return checked(literal[2]);
      }),
    );
  }
  for (const match of code.matchAll(/(?<!function\s)\brequireSuccessfulPushRun\(\s*(["'])([^"']+)\1\s*\)/g)) sites.push([checked(match[2])]);
  if (mentions - definitions === 0) throw new Error("no call site of requireSuccessfulPushRun was found");
  if (sites.length !== mentions - definitions) {
    throw new Error("a requireSuccessfulPushRun call site could not be read: it must be a literal call or a `for (const w of [\"a.yml\", ...])` loop");
  }
  return { workflows: [...new Set(sites.flat())].sort(), sites };
}

/**
 * @typedef {{ workflow: string, job: string, step: string | null, stepKeys: string[], key: string, message: string }} GateViolation
 */

/** @param {YamlNode | undefined} node @returns {node is Map<string, YamlNode>} */
const isMap = (node) => node instanceof Map;

/** @param {YamlNode | undefined} node @returns {boolean} */
const isLiteralFalse = (node) => node !== undefined && !Array.isArray(node) && !(node instanceof Map) && node.kind === "scalar" && !node.quoted && node.value === "false";

/** Condition text of an `if:` node, whitespace-folded, with one `${{ }}` wrapper removed. */
function conditionText(/** @type {YamlNode} */ node) {
  if (!node || Array.isArray(node) || node instanceof Map || node.kind !== "scalar") return null;
  let text = node.value.replace(/\s+/g, " ").trim();
  const wrapped = /^\$\{\{\s*(.*?)\s*\}\}$/.exec(text);
  if (wrapped && !wrapped[1].includes("${{") && !wrapped[1].includes("}}")) text = wrapped[1];
  return text.includes("${{") || text.includes("}}") ? null : text;
}

/**
 * Values the job's matrix gives each axis, for checking a platform selector
 * against the legs that exist.
 *
 * @param {Map<string, YamlNode>} job
 * @returns {Map<string, string[]>}
 */
function matrixAxes(job) {
  /** @type {Map<string, string[]>} */
  const axes = new Map();
  const strategy = job.get("strategy");
  const matrix = isMap(strategy) ? strategy.get("matrix") : undefined;
  if (!isMap(matrix)) return axes;
  for (const [axis, node] of matrix) {
    if (Array.isArray(node)) {
      const values = node.flatMap((item) => (!Array.isArray(item) && !isMap(item) && item.kind === "scalar" ? [item.value] : []));
      if (values.length === node.length) axes.set(axis, values);
    } else if (!isMap(node) && node.kind === "flow" && node.items) axes.set(axis, node.items);
  }
  return axes;
}

const PLATFORM_TERM = /^(matrix\.[A-Za-z_][\w-]*|runner\.os)\s*(==|!=)\s*'([A-Za-z0-9._-]+)'$/;
const RUNNER_OS = ["linux", "macos", "windows"];

/**
 * Why a step condition is not one of the forms that cannot hide a failure on
 * every platform at once, or null when it is. Accepted: `!cancelled()` and
 * `matrix.<axis>` / `runner.os` compared with `==` or `!=` to a literal that
 * selects at least one leg and not all legs' complement, joined by `&&`. Any
 * condition this does not recognise is rejected.
 *
 * @param {string | null} text
 * @param {Map<string, string[]>} axes
 * @returns {string | null}
 */
function stepConditionProblem(text, axes) {
  if (text === null || text === "") return "its condition could not be read";
  for (const term of text.split("&&").map((part) => part.trim())) {
    if (term === "!cancelled()") continue;
    const platform = PLATFORM_TERM.exec(term);
    if (!platform) return `\`${term}\` is not \`!cancelled()\` or a platform selector (matrix.<axis> / runner.os ==|!= 'literal')`;
    const [, subject, operator, literal] = platform;
    const wanted = literal.toLowerCase();
    if (subject === "runner.os") {
      if (!RUNNER_OS.includes(wanted)) return `\`${term}\` compares runner.os with a value no runner reports`;
      continue;
    }
    const values = axes.get(subject.slice("matrix.".length));
    if (!values) return `\`${term}\` names ${subject}, which this job's matrix does not define as a list`;
    const lower = values.map((value) => value.toLowerCase());
    if (operator === "==" && !lower.includes(wanted)) return `\`${term}\` matches none of the job's legs (${values.join(", ")}), so the step never runs`;
    if (operator === "!=" && lower.every((value) => value === wanted)) return `\`${term}\` excludes every one of the job's legs, so the step never runs`;
  }
  return null;
}

/**
 * Every way a job or step in one workflow can be skipped, or can fail without
 * failing its job.
 *
 * @param {string} workflow
 * @param {Map<string, YamlNode>} jobs
 * @returns {GateViolation[]}
 */
function skipViolations(workflow, jobs) {
  /** @type {GateViolation[]} */
  const violations = [];
  /** @type {Map<string, string[]>} */
  const needsOf = new Map();
  /** @type {Set<string>} */
  const ownSkips = new Set();

  /** @param {string} job @param {string | null} step @param {string[]} stepKeys @param {string} key @param {string} what */
  const add = (job, step, stepKeys, key, what) => {
    const where = `${workflow}: job "${job}"${step === null ? "" : ` step ${step}`}`;
    violations.push({ workflow, job, step, stepKeys, key, message: `${where}: ${what}; ${SKIP_HINT}` });
  };

  for (const [jobId, job] of jobs) {
    if (!isMap(job)) {
      add(jobId, null, [], "unreadable", "the job is not a mapping, so it cannot be checked");
      continue;
    }
    if (job.has("if")) {
      ownSkips.add(jobId);
      add(jobId, null, [], "if", "job-level `if` can skip the job, and a skipped job leaves the run concluding success");
    }
    if (job.has("continue-on-error") && !isLiteralFalse(job.get("continue-on-error"))) {
      add(jobId, null, [], "continue-on-error", "job-level `continue-on-error` lets the job fail while the run concludes success");
    }
    const needs = job.get("needs");
    if (needs !== undefined) {
      const names = Array.isArray(needs)
        ? needs.map((item) => (!Array.isArray(item) && !isMap(item) && item.kind === "scalar" ? item.value : null))
        : !isMap(needs) && needs.kind === "scalar"
          ? [needs.value]
          : !isMap(needs) && needs.kind === "flow" && needs.items
            ? needs.items
            : [null];
      needsOf.set(jobId, names.filter((name) => name !== null));
      if (names.includes(null)) add(jobId, null, [], "needs", "`needs` could not be read");
    }
    const strategy = job.get("strategy");
    if (isMap(strategy) && strategy.has("matrix")) {
      const matrix = strategy.get("matrix");
      if (!isMap(matrix)) add(jobId, null, [], "matrix.exclude", "`strategy.matrix` is not a mapping, so its legs cannot be checked");
      else if (matrix.has("exclude")) add(jobId, null, [], "matrix.exclude", "`matrix.exclude` removes legs from the matrix without a trace");
    }
    const steps = job.get("steps");
    if (steps === undefined) continue;
    if (!Array.isArray(steps)) {
      add(jobId, null, [], "unreadable", "`steps` is not a list, so the steps cannot be checked");
      continue;
    }
    const axes = matrixAxes(job);
    const names = steps.map((step) => {
      const name = isMap(step) ? step.get("name") : undefined;
      return name && !isMap(name) && !Array.isArray(name) && name.kind === "scalar" && name.value !== "" ? name.value : null;
    });
    steps.forEach((step, index) => {
      const unique = names[index] !== null && names.filter((name) => name === names[index]).length === 1;
      const label = unique ? `"${names[index]}"` : `#${index}`;
      const keys = [`#${index}`, ...(unique && names[index] !== null ? [/** @type {string} */ (names[index])] : [])];
      if (!isMap(step)) {
        add(jobId, label, keys, "unreadable", "the step is not a mapping, so it cannot be checked");
        return;
      }
      if (step.has("continue-on-error") && !isLiteralFalse(step.get("continue-on-error"))) {
        add(jobId, label, keys, "continue-on-error", "step-level `continue-on-error` lets the step fail without failing the job");
      }
      if (step.has("if")) {
        const problem = stepConditionProblem(conditionText(/** @type {YamlNode} */ (step.get("if"))), axes);
        if (problem) add(jobId, label, keys, "if", `step-level \`if\` can skip the step on every platform at once: ${problem}`);
      }
    });
  }

  // A job that needs a job that can skip is skipped with it. Propagate to a
  // fixed point so a chain of dependents is reported at every link.
  const skippable = new Set(ownSkips);
  for (let changed = true; changed;) {
    changed = false;
    for (const [jobId, needed] of needsOf) {
      if (!skippable.has(jobId) && needed.some((name) => skippable.has(name))) {
        skippable.add(jobId);
        changed = true;
      }
    }
  }
  for (const [jobId, needed] of needsOf) {
    for (const name of needed) {
      if (!jobs.has(name)) add(jobId, null, [], "needs", `\`needs\` names "${name}", which is not a job in this workflow`);
      else if (skippable.has(name)) add(jobId, null, [], "needs", `\`needs\` includes "${name}", which can be skipped, and a job whose dependency was skipped is skipped too`);
    }
  }
  return violations;
}

/**
 * Applies the release-gate rule to the trusted workflows' texts (null for a
 * file that could not be read) and returns one line per finding: a job or step
 * that can skip, a stale or malformed allowlist entry, or a workflow that could
 * not be read or parsed.
 *
 * @param {Record<string, string | null>} texts  trusted workflow file name -> contents
 * @param {ReleaseGateAllowEntry[]} [allowlist]
 * @returns {string[]}
 */
export function gateFindings(texts, allowlist = RELEASE_GATE_ALLOWLIST) {
  /** @type {string[]} */
  const findings = [];
  /** @type {Set<string>} */
  const unjudged = new Set();
  /** @type {GateViolation[]} */
  const violations = [];
  for (const [workflow, text] of Object.entries(texts)) {
    if (text === null) {
      findings.push(`${workflow}: trusted by the release gate but the file cannot be read; the rule could not run on it`);
      unjudged.add(workflow);
      continue;
    }
    try {
      const root = parseWorkflowYaml(text);
      const jobs = isMap(root) ? root.get("jobs") : undefined;
      if (!isMap(jobs) || jobs.size === 0) throw new YamlError("there is no `jobs:` mapping with at least one job");
      violations.push(...skipViolations(workflow, jobs));
    } catch (error) {
      if (!(error instanceof YamlError)) throw error;
      findings.push(`${workflow}: trusted by the release gate but cannot be parsed (${error.message}); the rule could not run on it`);
      unjudged.add(workflow);
    }
  }

  /** @type {Set<number>} */
  const used = new Set();
  const usable = allowlist.map((entry, index) => {
    const label = `allowlist entry #${index} (${JSON.stringify(entry)})`;
    const shapeOk =
      typeof entry?.workflow === "string" && entry.workflow !== "" &&
      typeof entry.job === "string" && entry.job !== "" &&
      typeof entry.key === "string" && ALLOW_KEYS.has(entry.key) &&
      (entry.step === undefined || (typeof entry.step === "string" && entry.step !== ""));
    if (!shapeOk) {
      findings.push(`${label} is malformed: it needs a workflow, a job, a key of ${[...ALLOW_KEYS].join(" | ")} and, optionally, a step`);
      return false;
    }
    if (typeof entry.reason !== "string" || entry.reason.trim() === "") {
      findings.push(`${label} has no reason; an exemption must say why it is safe, so it is not honoured`);
      return false;
    }
    return true;
  });

  for (const violation of violations) {
    const index = allowlist.findIndex(
      (entry, at) =>
        usable[at] &&
        entry.workflow === violation.workflow &&
        entry.job === violation.job &&
        entry.key === violation.key &&
        (entry.step === undefined ? violation.step === null : violation.stepKeys.includes(entry.step)),
    );
    if (index === -1) findings.push(violation.message);
    else used.add(index);
  }
  allowlist.forEach((entry, index) => {
    if (usable[index] && !used.has(index) && !unjudged.has(entry.workflow)) {
      findings.push(
        `stale allowlist entry: ${entry.workflow} job "${entry.job}"${entry.step === undefined ? "" : ` step ${JSON.stringify(entry.step)}`} key \`${entry.key}\` matches nothing in the workflows; remove it`,
      );
    }
  });
  return findings;
}

/**
 * The release-gate rule over the workflows `release-state.mjs` trusts.
 *
 * @param {string} dir  the workflows directory
 * @param {string} releaseStatePath  path to release-state.mjs
 * @param {ReleaseGateAllowEntry[]} [allowlist]
 * @returns {string[]}
 */
export function releaseGateFindings(dir, releaseStatePath, allowlist = RELEASE_GATE_ALLOWLIST) {
  /** @type {string[]} */
  let trusted;
  try {
    trusted = trustedWorkflowNames(readFileSync(releaseStatePath, "utf8")).workflows;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    return [`could not determine the trusted workflows from ${path.basename(releaseStatePath)} (${message}); the release-gate rule did not run`];
  }
  /** @type {Record<string, string | null>} */
  const texts = {};
  for (const name of trusted) {
    try {
      texts[name] = readFileSync(path.join(dir, name), "utf8");
    } catch {
      texts[name] = null;
    }
  }
  return gateFindings(texts, allowlist);
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
  const findings = [
    ...workflowPolicyFindings(WORKFLOW_DIR),
    ...releaseGateFindings(WORKFLOW_DIR, RELEASE_STATE_PATH),
  ];
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
    console.log(`OK: ${files.length} workflow files lint clean, every action pinned to a commit, no checkout keeps its token, no download is piped into a shell, no check the release gate trusts can skip (${files.join(", ")})`);
  }
  return code;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
