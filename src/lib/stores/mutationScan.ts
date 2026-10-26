import { dirname, join, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "svelte/compiler";
import ts from "typescript";

/**
 * The scanner behind `repoStoreOutcome.contract.test.ts`: which repoStore
 * members are mutations, and which calls to them drop their outcome.
 *
 * Built on the TypeScript compiler API rather than on text, so the shapes a
 * regular expression cannot see — an aliased store, a destructured method, an
 * optional call, `repoStore["x"]`, a `.then` chain, a `Promise.all` over
 * mutations, a floating call — are all read from the syntax tree.
 */

const STORES = dirname(fileURLToPath(import.meta.url));
// In `/` form: the compiler hands its host `/`-separated names on every
// platform, so a `\` path from `join` would never match the override below on
// Windows and a synthetic store source would silently be read from disk.
const STORE_PATH = join(STORES, "repoStore.ts").split(sep).join("/");
const ROOT = join(STORES, "..", "..", "..");

/** The position of the trailing `target` parameter of each member that takes one. */
export const TARGET_ARG_INDEX: Readonly<Record<string, number>> = {
  discardChanges: 1,
  createBranch: 2,
  renameBranch: 2,
  deleteBranch: 2,
  resetTo: 2,
  createTag: 3,
  deleteTag: 1,
  createFile: 1,
  movePath: 2,
  deletePath: 1,
  pruneWorktrees: 0,
  addWorktree: 1,
  removeWorktree: 2,
  lockWorktree: 1,
  unlockWorktree: 1,
  worktreeMergeTeardown: 1,
  worktreeCowSync: 1,
  deadbranchClean: 3,
  deadbranchRestore: 1,
  fetch: 1,
  pull: 3,
  push: 3,
  stashSave: 2,
  stashPop: 0,
};

const PROMPTS = new Set(["askConfirm", "askText"]);
const PROMISE_COMBINATORS = new Set(["all", "allSettled", "race", "any"]);

function parseSource(name: string, text: string): ts.SourceFile {
  return ts.createSourceFile(name, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
}

function lineOf(sf: ts.SourceFile, node: ts.Node): number {
  return sf.getLineAndCharacterOfPosition(node.getStart(sf)).line + 1;
}

function unwrap(node: ts.Expression): ts.Expression {
  let current = node;
  while (
    ts.isParenthesizedExpression(current) ||
    ts.isNonNullExpression(current) ||
    ts.isAsExpression(current) ||
    ts.isSatisfiesExpression(current)
  ) {
    current = current.expression;
  }
  return current;
}

// ---- which members are mutations ------------------------------------------

let realOptions: ts.CompilerOptions | null = null;
function compilerOptions(): ts.CompilerOptions {
  if (realOptions) return realOptions;
  const read = ts.readConfigFile(join(ROOT, "tsconfig.json"), ts.sys.readFile);
  const parsed = ts.parseJsonConfigFileContent(read.config, ts.sys, ROOT);
  realOptions = { ...parsed.options, noEmit: true, skipLibCheck: true, types: ["node"] };
  return realOptions;
}

const derived = new Map<string, Set<string>>();

/** Members of the store literal whose result is a `MutationOutcome`. */
function storeLiteral(sf: ts.SourceFile): ts.ObjectLiteralExpression | null {
  let found: ts.ObjectLiteralExpression | null = null;
  const visit = (node: ts.Node) => {
    if (found) return;
    if (
      ts.isVariableDeclaration(node) &&
      ts.isIdentifier(node.name) &&
      node.name.text === "store" &&
      node.initializer &&
      ts.isObjectLiteralExpression(node.initializer)
    ) {
      found = node.initializer;
      return;
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return found;
}

function returnedExpressions(fn: ts.FunctionLikeDeclaration): ts.Expression[] {
  if (!fn.body) return [];
  if (!ts.isBlock(fn.body)) return [fn.body];
  const out: ts.Expression[] = [];
  const visit = (node: ts.Node) => {
    if (ts.isFunctionLike(node)) return;
    if (ts.isReturnStatement(node) && node.expression) out.push(node.expression);
    ts.forEachChild(node, visit);
  };
  visit(fn.body);
  return out;
}

/**
 * The names of the store's mutations: members whose return type is
 * `MutationOutcome` or `Promise<MutationOutcome>`, read from the type checker.
 *
 * A member whose result comes from a call the checker cannot resolve is a
 * mutation too. That never happens in repoStore.ts itself — every callee there
 * is typed — so it only decides a source that is not type-complete, and it
 * decides it toward flagging: a check that could not run must not pass.
 */
export function mutationNames(storeSource: string): Set<string> {
  const cached = derived.get(storeSource);
  if (cached) return cached;
  const complete = /^\s*import\s/m.test(storeSource);
  const options: ts.CompilerOptions = complete
    ? compilerOptions()
    : { noLib: true, noResolve: true, noEmit: true, target: ts.ScriptTarget.ESNext };
  const host = ts.createCompilerHost(options);
  const getSourceFile = host.getSourceFile.bind(host);
  host.getSourceFile = (name, languageVersion, onError, shouldCreate) =>
    name === STORE_PATH
      ? ts.createSourceFile(name, storeSource, languageVersion, true, ts.ScriptKind.TS)
      : getSourceFile(name, languageVersion, onError, shouldCreate);
  const program = ts.createProgram({ rootNames: [STORE_PATH], options, host });
  const checker = program.getTypeChecker();
  const sf = program.getSourceFile(STORE_PATH);
  const names = new Set<string>();
  const literal = sf ? storeLiteral(sf) : null;
  if (!literal) return names;

  const isOutcomeType = (type: ts.Type) => checker.typeToString(type).includes("MutationOutcome");
  for (const member of literal.properties) {
    if (!member.name || !(ts.isIdentifier(member.name) || ts.isStringLiteral(member.name))) continue;
    let fn: ts.FunctionLikeDeclaration | null = null;
    if (ts.isMethodDeclaration(member)) fn = member;
    else if (ts.isPropertyAssignment(member) && (ts.isArrowFunction(member.initializer) || ts.isFunctionExpression(member.initializer))) {
      fn = member.initializer;
    }
    if (!fn) continue;
    const signature = checker.getSignatureFromDeclaration(fn);
    let mutation = signature ? isOutcomeType(checker.getReturnTypeOfSignature(signature)) : false;
    if (!mutation) {
      mutation = returnedExpressions(fn).some((expression) => {
        const inner = unwrap(ts.isAwaitExpression(expression) ? expression.expression : expression);
        if (!ts.isCallExpression(inner)) return false;
        const callee = unwrap(inner.expression);
        return ts.isIdentifier(callee) && checker.getSymbolAtLocation(callee) === undefined;
      });
    }
    if (mutation) names.add(member.name.text);
  }
  derived.set(storeSource, names);
  return names;
}

// ---- which calls drop their outcome ---------------------------------------

interface Aliases {
  /** Identifiers that are the store itself: `repoStore` and what it was assigned to. */
  stores: Set<string>;
  /** Identifiers bound to one of its mutation methods, mapped to the member's name. */
  methods: Map<string, string>;
  /** Local names of askConfirm / askText, including import aliases. */
  prompts: Set<string>;
}

const newAliases = (): Aliases => ({ stores: new Set(["repoStore"]), methods: new Map(), prompts: new Set(PROMPTS) });

/** The mutation a callee names, `"*"` for one that cannot be named, or null. */
function mutationOf(callee: ts.Expression, aliases: Aliases, names: ReadonlySet<string>): string | null {
  const target = unwrap(callee);
  if (ts.isIdentifier(target)) return aliases.methods.get(target.text) ?? null;
  if (ts.isPropertyAccessExpression(target)) {
    const owner = unwrap(target.expression);
    return ts.isIdentifier(owner) && aliases.stores.has(owner.text) && names.has(target.name.text) ? target.name.text : null;
  }
  if (ts.isElementAccessExpression(target)) {
    const owner = unwrap(target.expression);
    if (!ts.isIdentifier(owner) || !aliases.stores.has(owner.text)) return null;
    const key = target.argumentExpression;
    if (ts.isStringLiteralLike(key)) {
      const text = key.text.trim();
      // A blanked string (the contract test's comment-and-string stripper) has
      // lost its name: count it as a mutation rather than miss one.
      if (text === "") return "*";
      return names.has(text) ? text : null;
    }
    return "*";
  }
  return null;
}

function collectAliases(
  sf: ts.SourceFile,
  names: ReadonlySet<string>,
  aliases: Aliases = newAliases(),
): Aliases {
  let grew = true;
  const visit = (node: ts.Node) => {
    if (ts.isImportSpecifier(node) && PROMPTS.has((node.propertyName ?? node.name).text) && !aliases.prompts.has(node.name.text)) {
      aliases.prompts.add(node.name.text);
      grew = true;
    }
    if (ts.isVariableDeclaration(node) && node.initializer) {
      const init = unwrap(node.initializer);
      if (ts.isIdentifier(node.name)) {
        if (ts.isIdentifier(init) && aliases.stores.has(init.text) && !aliases.stores.has(node.name.text)) {
          aliases.stores.add(node.name.text);
          grew = true;
        } else if (ts.isIdentifier(init) && aliases.prompts.has(init.text) && !aliases.prompts.has(node.name.text)) {
          aliases.prompts.add(node.name.text);
          grew = true;
        } else if (
          (ts.isPropertyAccessExpression(init) || ts.isElementAccessExpression(init)) &&
          !aliases.methods.has(node.name.text)
        ) {
          const member = mutationOf(init, aliases, names);
          if (member !== null) {
            aliases.methods.set(node.name.text, member);
            grew = true;
          }
        }
      } else if (ts.isObjectBindingPattern(node.name) && ts.isIdentifier(init) && aliases.stores.has(init.text)) {
        for (const element of node.name.elements) {
          const source = element.propertyName ?? element.name;
          const sourceName = ts.isIdentifier(source) || ts.isStringLiteralLike(source) ? source.text : "";
          if (ts.isIdentifier(element.name) && names.has(sourceName) && !aliases.methods.has(element.name.text)) {
            aliases.methods.set(element.name.text, sourceName);
            grew = true;
          }
        }
      }
    }
    ts.forEachChild(node, visit);
  };
  while (grew) {
    grew = false;
    visit(sf);
  }
  return aliases;
}

function isPromiseCombinator(call: ts.CallExpression): boolean {
  const callee = unwrap(call.expression);
  return (
    ts.isPropertyAccessExpression(callee) &&
    ts.isIdentifier(callee.expression) &&
    callee.expression.text === "Promise" &&
    PROMISE_COMBINATORS.has(callee.name.text)
  );
}

/** Whether the value of `call` reaches nothing that can read it. */
function isDiscarded(call: ts.CallExpression, handler = false): boolean {
  let node: ts.Node = call;
  for (;;) {
    const parent: ts.Node = node.parent;
    if (!parent) return false;
    if (
      ts.isAwaitExpression(parent) ||
      ts.isParenthesizedExpression(parent) ||
      ts.isNonNullExpression(parent) ||
      ts.isAsExpression(parent) ||
      ts.isSatisfiesExpression(parent)
    ) {
      node = parent;
      continue;
    }
    if (ts.isBinaryExpression(parent)) {
      const op = parent.operatorToken.kind;
      if (op === ts.SyntaxKind.CommaToken) {
        if (parent.left === node) return true;
        node = parent;
        continue;
      }
      if (
        (op === ts.SyntaxKind.AmpersandAmpersandToken ||
          op === ts.SyntaxKind.BarBarToken ||
          op === ts.SyntaxKind.QuestionQuestionToken) &&
        parent.right === node
      ) {
        node = parent;
        continue;
      }
      return false;
    }
    if (ts.isConditionalExpression(parent) && parent.condition !== node) {
      node = parent;
      continue;
    }
    if (ts.isExpressionStatement(parent)) return true;
    if (ts.isVoidExpression(parent)) return false;
    if (ts.isPropertyAccessExpression(parent) && parent.expression === node) {
      const outer = parent.parent;
      if (!ts.isCallExpression(outer) || outer.expression !== parent) return false;
      const chained = parent.name.text;
      if (chained === "then") {
        const callback = outer.arguments[0];
        const reads = callback && ts.isFunctionLike(callback) && callback.parameters.length > 0;
        return !reads;
      }
      // `catch` and `finally` never see the outcome: a mutation resolves, it does not reject.
      return chained === "catch" || chained === "finally";
    }
    if (ts.isArrayLiteralExpression(parent)) {
      node = parent;
      continue;
    }
    if (ts.isCallExpression(parent) && parent.arguments.some((argument) => argument === node) && isPromiseCombinator(parent)) {
      node = parent;
      continue;
    }
    if (ts.isArrowFunction(parent) && parent.body === node) {
      const user = parent.parent;
      if (ts.isCallExpression(user) && user.arguments.some((argument) => argument === parent)) {
        const callee = unwrap(user.expression);
        const via = ts.isPropertyAccessExpression(callee) ? callee.name.text : "";
        if (via === "forEach") return true;
        if (via === "map" || via === "flatMap") {
          node = user;
          continue;
        }
      }
      // An arrow that IS an event handler (`onclick={() => repoStore.x()}`)
      // returns its value to the event system, which reads nothing.
      return handler && ts.isParenthesizedExpression(parent.parent) && ts.isExpressionStatement(parent.parent.parent);
    }
    return false;
  }
}

/** Lines of calls to a mutation whose value is dropped. */
export function discardedMutations(code: string, names: ReadonlySet<string>): number[] {
  return discardedIn(parseSource("scan.ts", code), names);
}

function discardedIn(
  sf: ts.SourceFile,
  names: ReadonlySet<string>,
  aliases: Aliases = collectAliases(sf, names),
  handler = false,
): number[] {
  const lines: number[] = [];
  const visit = (node: ts.Node) => {
    if (ts.isCallExpression(node) && mutationOf(node.expression, aliases, names) !== null && isDiscarded(node, handler)) {
      lines.push(lineOf(sf, node));
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return lines;
}

/** Whether a prompt, or a toast's action, stands between the start of the flow and `call`. */
function followsPrompt(call: ts.CallExpression, aliases: Aliases): boolean {
  for (let node: ts.Node | undefined = call.parent; node; node = node.parent) {
    if (!ts.isFunctionLike(node)) continue;
    const user = node.parent;
    if (ts.isCallExpression(user) && user.arguments.some((argument) => argument === node)) {
      const callee = unwrap(user.expression);
      if (ts.isPropertyAccessExpression(callee) && callee.name.text === "action") return true;
    }
    let prompted = false;
    const visit = (child: ts.Node) => {
      if (prompted) return;
      if (ts.isAwaitExpression(child) && child.end <= call.getStart()) {
        const asked = unwrap(child.expression);
        if (ts.isCallExpression(asked) && ts.isIdentifier(asked.expression) && aliases.prompts.has(asked.expression.text)) prompted = true;
      }
      ts.forEachChild(child, visit);
    };
    const body = (node as ts.FunctionLikeDeclaration).body;
    if (body) visit(body);
    if (prompted) return true;
  }
  return false;
}

/**
 * Lines of a `repoStore.<mutation>(…)` that follows a prompt, a confirm or a
 * toast action and names no target. Such a call runs on whichever tab is
 * active when the answer arrives.
 */
export function untargetedAfterPrompt(code: string): number[] {
  const sf = parseSource("scan.ts", code);
  const names = new Set(Object.keys(TARGET_ARG_INDEX));
  return untargetedIn(sf, names, collectAliases(sf, names));
}

function untargetedIn(sf: ts.SourceFile, names: ReadonlySet<string>, aliases: Aliases): number[] {
  const lines: number[] = [];
  const visit = (node: ts.Node) => {
    if (ts.isCallExpression(node)) {
      // Through aliases too: `const s = repoStore`, `const { createBranch } = repoStore`.
      const member = mutationOf(node.expression, aliases, names);
      if (member !== null && Object.hasOwn(TARGET_ARG_INDEX, member)) {
        const at = TARGET_ARG_INDEX[member];
        const given = node.arguments[at];
        const absent = given === undefined || (ts.isIdentifier(given) && given.text === "undefined");
        if (absent && followsPrompt(node, aliases)) lines.push(lineOf(sf, node));
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return lines;
}

// ---- direct invokes of a mutating command ------------------------------------

/** The text of a string-literal argument, or null for anything computed. */
function literalText(node: ts.Node | undefined): string | null {
  if (!node) return null;
  const inner = unwrap(node as ts.Expression);
  return ts.isStringLiteral(inner) || ts.isNoSubstitutionTemplateLiteral(inner) ? inner.text : null;
}

/**
 * The `cmd_*` commands the store's mutations send: every command named by a
 * string literal inside a member that `mutationNames` derives. A component
 * that sends one of these itself has stepped around the wrapper — the quit
 * guard, the per-repository queue, the journal and the echo rule — so the set
 * follows the store: a mutation added there is covered the day it is written.
 */
export function mutatingCommands(storeSource: string): Set<string> {
  const names = mutationNames(storeSource);
  const commands = new Set<string>();
  const sf = parseSource("repoStore.ts", storeSource);
  const literal = storeLiteral(sf);
  if (!literal) return commands;
  const collect = (node: ts.Node) => {
    if (ts.isCallExpression(node)) {
      const command = literalText(node.arguments[0]);
      if (command !== null && command.startsWith("cmd_")) commands.add(command);
    }
    ts.forEachChild(node, collect);
  };
  for (const member of literal.properties) {
    if (member.name && (ts.isIdentifier(member.name) || ts.isStringLiteral(member.name)) && names.has(member.name.text)) collect(member);
  }
  return commands;
}

/** The names the IPC bridge is exported under (`ipc/invoke`, `@tauri-apps/api/core`), before any rename. */
const BRIDGE_NAME = /^(invoke|invokeFn)$/;

/**
 * What a source file says about the IPC bridge: string constants a command
 * name may be read from, and the identifiers that ARE the bridge.
 */
interface BridgeScope {
  consts: Map<string, string>;
  bindings: Set<string>;
}

/** Whether `expr` names the bridge: `invoke`, `x.invoke`, `x["invoke"]`, or a bound alias. */
function isBridge(expr: ts.Expression, scope: BridgeScope): boolean {
  const target = unwrap(expr);
  if (ts.isIdentifier(target)) return scope.bindings.has(target.text) || BRIDGE_NAME.test(target.text);
  if (ts.isPropertyAccessExpression(target)) return BRIDGE_NAME.test(target.name.text);
  if (ts.isElementAccessExpression(target)) {
    const key = literalText(target.argumentExpression);
    return key !== null && BRIDGE_NAME.test(key);
  }
  return false;
}

function collectBridge(sfs: readonly ts.SourceFile[]): BridgeScope {
  const scope: BridgeScope = { consts: new Map(), bindings: new Set() };
  let grew = true;
  const visit = (node: ts.Node) => {
    if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier) && node.importClause?.namedBindings && ts.isNamedImports(node.importClause.namedBindings)) {
      for (const element of node.importClause.namedBindings.elements) {
        if (BRIDGE_NAME.test((element.propertyName ?? element.name).text) &&!scope.bindings.has(element.name.text)) {
          scope.bindings.add(element.name.text);
          grew = true;
        }
      }
    }
    if (ts.isVariableDeclaration(node) && node.initializer) {
      const init = unwrap(node.initializer);
      if (ts.isIdentifier(node.name)) {
        const text = literalText(init);
        if (text !== null) scope.consts.set(node.name.text, text);
        else if (isBridge(init, scope) && !scope.bindings.has(node.name.text)) {
          scope.bindings.add(node.name.text);
          grew = true;
        }
      } else if (ts.isObjectBindingPattern(node.name)) {
        // `const { invoke: go } = await import("@tauri-apps/api/core")` and `= window.__TAURI__.core`.
        for (const element of node.name.elements) {
          const source = element.propertyName ?? element.name;
          if (ts.isIdentifier(element.name) && ts.isIdentifier(source) && BRIDGE_NAME.test(source.text) && !scope.bindings.has(element.name.text)) {
            scope.bindings.add(element.name.text);
            grew = true;
          }
        }
      }
    }
    ts.forEachChild(node, visit);
  };
  while (grew) {
    grew = false;
    for (const sf of sfs) visit(sf);
  }
  return scope;
}

/**
 * What an argument says about the command it names: its whole text when it is
 * readable (a literal, or a constant in this file), the static prefix of one
 * that is built (`"cmd_" + x`, a template with substitutions), or neither.
 */
function commandOf(node: ts.Node | undefined, scope: BridgeScope): { text?: string; prefix?: string } {
  if (!node) return {};
  const inner = unwrap(node as ts.Expression);
  const literal = literalText(inner);
  if (literal !== null) return { text: literal };
  if (ts.isIdentifier(inner)) {
    const held = scope.consts.get(inner.text);
    return held === undefined ? {} : { text: held };
  }
  if (ts.isTemplateExpression(inner)) return { prefix: inner.head.text };
  if (ts.isBinaryExpression(inner) && inner.operatorToken.kind === ts.SyntaxKind.PlusToken) {
    const left = commandOf(inner.left, scope);
    return left.text !== undefined ? { prefix: left.text } : left;
  }
  return {};
}

function directInvokesIn(sf: ts.SourceFile, commands: ReadonlySet<string>, scope: BridgeScope = collectBridge([sf])): number[] {
  const lines: number[] = [];
  const visit = (node: ts.Node) => {
    if (ts.isCallExpression(node)) {
      const bridge = isBridge(node.expression, scope);
      const flagged = node.arguments.some((argument, index) => {
        const { text, prefix } = commandOf(argument, scope);
        if (text !== undefined) return commands.has(text);
        if (prefix !== undefined && prefix.startsWith("cmd_")) return [...commands].some((command) => command.startsWith(prefix));
        // The bridge called with a command that cannot be read: it may be any.
        return bridge && index === 0 && !ts.isStringLiteralLike(unwrap(argument));
      });
      if (flagged) lines.push(lineOf(sf, node));
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return lines;
}

/**
 * Lines of a call that sends a mutating command: one with a command's name in
 * any argument, or the IPC bridge called with a name that cannot be read.
 *
 * The callee is not what is read, so a direct, generic, aliased
 * (`import { invoke as call }`), `window.__TAURI__.core.invoke`, dynamically
 * imported or wrapped call are all alike. A name held in a same-file constant
 * is resolved; one built from a `cmd_` prefix matches every command that
 * starts with it; any other first argument to something that IS the bridge is
 * flagged, because it may name a mutation. Calls that are not the bridge are
 * read only for literal names, so an ordinary function is not flagged.
 *
 * Blind to a command the store does not send: the set comes from repoStore, so
 * a mutating `cmd_*` that has no wrapper there (WorktreesPanel's add, remove,
 * lock, unlock and merge-and-teardown) is invisible until the store owns it.
 */
export function directMutatingInvokes(code: string, commands: ReadonlySet<string>): number[] {
  return directInvokesIn(parseSource("scan.ts", code), commands);
}

/** The same scan over a component's scripts and markup expressions. */
export function directMutatingInvokesSvelte(source: string, commands: ReadonlySet<string>): number[] {
  if (!/cmd_|invoke/i.test(source)) return [];
  const { scripts, expressions } = svelteRanges(source);
  const scriptFiles = scripts.map((range) => parseSource("component.ts", padding(source.slice(0, range.start)) + source.slice(range.start, range.end)));
  // The markup sees what the scripts bound and held.
  const scope = collectBridge(scriptFiles);
  const mentions = ["cmd_", "invoke", ...scope.bindings, ...scope.consts.keys()];
  const markupFiles = expressions
    .map((range) => ({ range, text: source.slice(range.start, range.end) }))
    .filter(({ text }) => mentions.some((name) => text.includes(name)))
    .map(({ range, text }) => parseSource("component.ts", `${padding(source.slice(0, range.start - 1))}(${text});`));
  const lines = [...scriptFiles, ...markupFiles].flatMap((sf) => directInvokesIn(sf, commands, scope));
  return [...new Set(lines)].sort((a, b) => a - b);
}

// ---- Svelte ----------------------------------------------------------------

interface Range {
  start: number;
  end: number;
}

interface HandlerRange extends Range {
  handler: boolean;
}

const isRange = (value: unknown): value is Range =>
  typeof value === "object" && value !== null &&
  typeof (value as Range).start === "number" && typeof (value as Range).end === "number";

/** The source ranges of a component's scripts and of every expression in its markup. */
function svelteRanges(source: string): { scripts: Range[]; expressions: HandlerRange[] } {
  const root = parse(source, { modern: true }) as unknown as Record<string, unknown>;
  const scripts: Range[] = [];
  for (const key of ["instance", "module"]) {
    const script = root[key] as { content?: Range } | null | undefined;
    if (script?.content && isRange(script.content)) scripts.push({ start: script.content.start, end: script.content.end });
  }
  const expressions = new Map<number, HandlerRange>();
  const seen = new Set<unknown>();
  // Template nodes are walked; the estree expressions hanging off them are
  // taken whole and never entered, so `void f()` is read as a unit.
  const templateType = /^(Fragment|Text|Comment|ExpressionTag|HtmlTag|ConstTag|DebugTag|RenderTag|AttachTag|Attribute|SpreadAttribute|\w+Directive|\w+Block|\w+Element|Component|Svelte\w+)$/;
  const walk = (value: unknown) => {
    if (typeof value !== "object" || value === null || seen.has(value)) return;
    seen.add(value);
    if (Array.isArray(value)) {
      value.forEach(walk);
      return;
    }
    const record = value as Record<string, unknown>;
    if (typeof record.type !== "string" || !templateType.test(record.type)) return;
    // An event attribute (`onclick={…}`) or directive (`on:click={…}`): its
    // expression is a handler, and a handler's return value goes nowhere.
    const handler =
      record.type === "OnDirective" ||
      (record.type === "Attribute" && typeof record.name === "string" && /^on/.test(record.name));
    for (const key of ["expression", "test"]) {
      const inner = record[key];
      if (isRange(inner) && !templateType.test(String((inner as { type?: unknown }).type))) {
        expressions.set(inner.start, { start: inner.start, end: inner.end, handler: expressions.get(inner.start)?.handler === true || record.type === "OnDirective" });
      }
    }
    if (handler && record.type === "Attribute") {
      const tag = record.value as { type?: string; expression?: unknown } | null | undefined;
      if (tag && !Array.isArray(tag) && tag.type === "ExpressionTag" && isRange(tag.expression)) {
        expressions.set(tag.expression.start, { start: tag.expression.start, end: tag.expression.end, handler: true });
      }
    }
    for (const [key, child] of Object.entries(record)) {
      if (key !== "expression" && key !== "test") walk(child);
    }
  };
  walk(root.fragment);
  return { scripts, expressions: [...expressions.values()] };
}

const padding = (text: string) => text.replace(/[^\n]/g, " ");

/** Lines (1-based, in the component's own numbering) of dropped outcomes and untargeted post-prompt calls. */
export function scanSvelte(source: string, names: ReadonlySet<string>): { discarded: number[]; untargeted: number[] } {
  const discarded: number[] = [];
  const untargeted: number[] = [];
  if (!source.includes("repoStore")) return { discarded, untargeted };
  const { scripts, expressions } = svelteRanges(source);
  const parsed = scripts.map((range) => ({
    sf: parseSource("component.ts", padding(source.slice(0, range.start)) + source.slice(range.start, range.end)),
    handler: false,
  }));
  // What the scripts alias is visible to the markup too.
  const aliases = newAliases();
  for (const { sf } of parsed) collectAliases(sf, names, aliases);
  const mentions = [...aliases.stores, ...aliases.methods.keys()];
  for (const range of expressions) {
    const text = source.slice(range.start, range.end);
    if (!mentions.some((name) => text.includes(name))) continue;
    parsed.push({
      sf: parseSource("component.ts", `${padding(source.slice(0, range.start - 1))}(${text});`),
      handler: range.handler,
    });
  }
  for (const { sf, handler } of parsed) {
    discarded.push(...discardedIn(sf, names, aliases, handler));
    untargeted.push(...untargetedIn(sf, names, aliases));
  }
  return { discarded: [...new Set(discarded)].sort((a, b) => a - b), untargeted: [...new Set(untargeted)].sort((a, b) => a - b) };
}

/** The same scan for a plain TypeScript module. */
export function scanTypeScript(source: string, names: ReadonlySet<string>): { discarded: number[]; untargeted: number[] } {
  const sf = parseSource("module.ts", source);
  const aliases = collectAliases(sf, names);
  return { discarded: discardedIn(sf, names, aliases), untargeted: untargetedIn(sf, names, aliases) };
}
