/**
 * Command-line tokenization and remediation-plan parsing for the terminal.
 *
 * GitPulse never spawns a shell: a typed line is tokenized into an argv and
 * handed to `cmd_terminal_run`, which executes argv[0] with the rest as
 * literal arguments. That removes whole classes of injection bugs, but it
 * also means shell syntax the backend cannot honor must be refused here —
 * loudly, before anything runs — rather than silently mis-executed.
 */

/** Characters that only make sense inside a shell. Their presence in a
 * typed line means the user asked for something the argv runner cannot do,
 * so the request is refused instead of mangled. */
const SHELL_METACHARACTERS = /[|&;<>()`$]/;

export interface TokenizeOk {
  ok: true;
  argv: string[];
}

export interface TokenizeError {
  ok: false;
  error: string;
}

export type TokenizeResult = TokenizeOk | TokenizeError;

const MAX_PLAN_LINES = 4_096;
const MAX_PLAN_STEPS = 255;
const MAX_COMMANDS_PER_STEP = 32;

/**
 * Splits one command line into argv, honoring single quotes, double quotes,
 * and backslash escapes. Returns a refusal when the line uses shell syntax
 * (pipes, chaining, redirection, substitution) that direct argv execution
 * cannot reproduce.
 *
 * Deliberately sh-like but minimal: no variable expansion, no globs (the
 * program sees the pattern literally), no comments.
 */
export function tokenizeCommand(line: string): TokenizeResult {
  const trimmed = line.trim();
  if (!trimmed) return { ok: false, error: "Type a command first." };

  const argv: string[] = [];
  let current = "";
  let hasToken = false;
  let quote: '"' | "'" | null = null;

  for (let i = 0; i < trimmed.length; i++) {
    const ch = trimmed[i];

    if (quote === "'") {
      if (ch === "'") quote = null;
      else current += ch;
      continue;
    }

    if (quote === '"') {
      if (ch === '"') {
        quote = null;
      } else if (ch === "\\" && i + 1 < trimmed.length) {
        const next = trimmed[i + 1];
        // Inside double quotes a backslash escapes only " and \ (and \ at
        // end-of-input); anything else stays literal, like a real shell.
        if (next === '"' || next === "\\" || next === "$" || next === "`") {
          current += next;
          i++;
        } else {
          current += ch;
        }
      } else {
        current += ch;
      }
      continue;
    }

    if (ch === "'" || ch === '"') {
      quote = ch;
      hasToken = true;
      continue;
    }

    if (ch === "\\" && i + 1 < trimmed.length) {
      current += trimmed[i + 1];
      i++;
      hasToken = true;
      continue;
    }

    if (ch === " " || ch === "\t") {
      if (hasToken) {
        argv.push(current);
        current = "";
        hasToken = false;
      }
      continue;
    }

    if (SHELL_METACHARACTERS.test(ch)) {
      return {
        ok: false,
        error:
          `"${describeMetacharacter(ch)}" is shell syntax; GitPulse runs one command ` +
          "at a time without a shell. Run the steps separately.",
      };
    }

    current += ch;
    hasToken = true;
  }

  if (quote !== null) {
    return { ok: false, error: "Unterminated quote — close the missing quotation mark." };
  }
  if (hasToken) argv.push(current);
  if (argv.length === 0 || !argv[0].trim()) {
    // An all-quoted empty token ("") would otherwise reach the backend as an
    // empty program name and fail there; refuse it here where the message
    // can point at the input line.
    return { ok: false, error: "Type a command first." };
  }
  return { ok: true, argv };
}

function describeMetacharacter(ch: string): string {
  switch (ch) {
    case "|":
      return "|";
    case "&":
      return "&";
    case ";":
      return ";";
    case "<":
    case ">":
      return `${ch} (redirection)`;
    case "(":
    case ")":
      return `${ch}`;
    case "`":
      return "` (command substitution)";
    case "$":
      return "$ (variable or substitution)";
    default:
      return ch;
  }
}

// ---------------------------------------------------------------------------
// Remediation-plan parsing (Health view → runnable steps)
// ---------------------------------------------------------------------------

export interface PlanStep {
  /** The plan's own step number, when the text numbered it. */
  number: number | null;
  /** The step's prose, with inline code spans kept intact. */
  text: string;
  /** Commands extracted from inline code spans on this line. */
  commands: string[];
}

/** One visible plan row after every command has been tokenized independently. */
export interface RunnablePlanStep {
  id: string;
  number: number | null;
  text: string;
  command: string | null;
  argv: string[] | null;
  error?: string;
}

/**
 * Extracts runnable steps from MANVI's rendered remediation plan.
 *
 * The fix prompt asks the model for a numbered plan whose commands appear in
 * inline code spans (`npm audit fix`). Models also wrap output in fences, so
 * fenced lines are treated like ordinary lines: their content is scanned for
 * code spans, and fence markers themselves are skipped. A step with no code
 * span is still listed — as prose the user can act on manually — because a
 * plan that lost its commands to model formatting must not silently vanish.
 */
export function extractPlanSteps(planText: string): PlanStep[] {
  const steps: PlanStep[] = [];
  let inFence = false;
  let fencedOwner: PlanStep | null = null;
  let capped = false;
  const lines = planText.split("\n");

  for (const rawLine of lines.slice(0, MAX_PLAN_LINES)) {
    const line = rawLine.trimEnd();
    const fence = line.match(/^\s*```/);
    if (fence) {
      inFence = !inFence;
      fencedOwner = inFence ? (steps.at(-1) ?? null) : null;
      continue;
    }

    if (inFence) {
      const command = line.trim();
      // A model often emits a language-labelled fenced block after a numbered
      // step. Keep each physical command as an independent argv candidate;
      // comments and blank lines are inert, and tokenization below still
      // rejects chaining, redirects and substitutions.
      if (fencedOwner && command && !command.startsWith("#")) {
        if (fencedOwner.commands.length < MAX_COMMANDS_PER_STEP) {
          fencedOwner.commands.push(command);
        } else {
          capped = true;
        }
      }
      continue;
    }

    const numbered = line.match(/^\s*(\d+)[.)]\s+(.*)$/);
    const bulleted = line.match(/^\s*[-*]\s+(.*)$/);
    if (!numbered && !bulleted) continue;

    if (steps.length >= MAX_PLAN_STEPS) {
      capped = true;
      break;
    }

    const body = numbered ? numbered[2] : bulleted![1];
    const inlineCommands = extractCodeSpans(body);
    if (inlineCommands.length > MAX_COMMANDS_PER_STEP) capped = true;
    steps.push({
      number: numbered ? Number.parseInt(numbered[1], 10) : null,
      text: body.trim(),
      commands: inlineCommands.slice(0, MAX_COMMANDS_PER_STEP),
    });
  }

  if (lines.length > MAX_PLAN_LINES) capped = true;
  if (capped) {
    steps.push({
      number: null,
      text: "Plan parsing was capped; additional steps were not treated as complete coverage.",
      commands: [],
    });
  }

  return steps;
}

/**
 * Converts model prose into visible, independently runnable rows.
 *
 * A step containing two commands becomes two rows so no command disappears.
 * A prose-only or rejected command stays visible with `argv: null`; callers
 * can explain why it did not run instead of silently dropping it.
 */
export function buildRunnablePlanSteps(planText: string): RunnablePlanStep[] {
  const runnable: RunnablePlanStep[] = [];
  for (const [stepIndex, step] of extractPlanSteps(planText).entries()) {
    const number = step.number ?? stepIndex + 1;
    if (step.commands.length === 0) {
      runnable.push({
        id: `step-${stepIndex + 1}-${number}-prose`,
        number,
        text: step.text,
        command: null,
        argv: null,
      });
      continue;
    }

    for (const [commandIndex, command] of step.commands.entries()) {
      const tokenized = tokenizeCommand(command);
      runnable.push({
        id: `step-${stepIndex + 1}-${number}-command-${commandIndex + 1}`,
        number,
        text: step.text,
        command,
        argv: tokenized.ok ? tokenized.argv : null,
        ...(tokenized.ok ? {} : { error: tokenized.error }),
      });
    }
  }
  return runnable;
}

/** Pulls the contents of `` `…` `` spans, skipping fenced-block markers. */
function extractCodeSpans(text: string): string[] {
  const commands: string[] = [];
  const re = /`([^`\n]+)`/g;
  let match: RegExpExecArray | null;
  while ((match = re.exec(text)) !== null) {
    const candidate = match[1].trim();
    // A span holding spaces-with-nothing-else or prose markers is noise;
    // anything else the model put in backticks on a step line is intended
    // to be runnable.
    if (candidate) commands.push(candidate);
    if (commands.length > MAX_COMMANDS_PER_STEP) break;
  }
  return commands;
}
