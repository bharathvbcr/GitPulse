import { dirname, join } from "node:path";
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
const STORE_PATH = join(STORES, "repoStore.ts");
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
  /** Identifiers bound to one of its mutation methods. */
  methods: Set<string>;
}

/** The mutation a callee names, `"*"` for one that cannot be named, or null. */
function mutationOf(callee: ts.Expression, aliases: Aliases, names: ReadonlySet<string>): string | null {
  const target = unwrap(callee);
  if (ts.isIdentifier(target)) return aliases.methods.has(target.text) ? target.text : null;
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
  aliases: Aliases = { stores: new Set(["repoStore"]), methods: new Set() },
): Aliases {
  let grew = true;
  const visit = (node: ts.Node) => {
    if (ts.isVariableDeclaration(node) && node.initializer) {
      const init = unwrap(node.initializer);
      if (ts.isIdentifier(node.name)) {
        if (ts.isIdentifier(init) && aliases.stores.has(init.text) && !aliases.stores.has(node.name.text)) {
          aliases.stores.add(node.name.text);
          grew = true;
        } else if (
          (ts.isPropertyAccessExpression(init) || ts.isElementAccessExpression(init)) &&
          mutationOf(init, aliases, names) !== null &&
          !aliases.methods.has(node.name.text)
        ) {
          aliases.methods.add(node.name.text);
          grew = true;
        }
      } else if (ts.isObjectBindingPattern(node.name) && ts.isIdentifier(init) && aliases.stores.has(init.text)) {
        for (const element of node.name.elements) {
          const source = element.propertyName ?? element.name;
          const sourceName = ts.isIdentifier(source) || ts.isStringLiteralLike(source) ? source.text : "";
          if (ts.isIdentifier(element.name) && names.has(sourceName) && !aliases.methods.has(element.name.text)) {
            aliases.methods.add(element.name.text);
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
function isDiscarded(call: ts.CallExpression): boolean {
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
      return false;
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
): number[] {
  const lines: number[] = [];
  const visit = (node: ts.Node) => {
    if (ts.isCallExpression(node) && mutationOf(node.expression, aliases, names) !== null && isDiscarded(node)) {
      lines.push(lineOf(sf, node));
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return lines;
}

/** Whether a prompt, or a toast's action, stands between the start of the flow and `call`. */
function followsPrompt(call: ts.CallExpression): boolean {
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
        if (ts.isCallExpression(asked) && ts.isIdentifier(asked.expression) && PROMPTS.has(asked.expression.text)) prompted = true;
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
  return untargetedIn(parseSource("scan.ts", code));
}

function untargetedIn(sf: ts.SourceFile): number[] {
  const lines: number[] = [];
  const visit = (node: ts.Node) => {
    if (ts.isCallExpression(node)) {
      const callee = unwrap(node.expression);
      if (
        ts.isPropertyAccessExpression(callee) &&
        ts.isIdentifier(callee.expression) &&
        callee.expression.text === "repoStore" &&
        Object.hasOwn(TARGET_ARG_INDEX, callee.name.text)
      ) {
        const at = TARGET_ARG_INDEX[callee.name.text];
        const given = node.arguments[at];
        const absent = given === undefined || (ts.isIdentifier(given) && given.text === "undefined");
        if (absent && followsPrompt(node)) lines.push(lineOf(sf, node));
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  return lines;
}

// ---- Svelte ----------------------------------------------------------------

interface Range {
  start: number;
  end: number;
}

const isRange = (value: unknown): value is Range =>
  typeof value === "object" && value !== null &&
  typeof (value as Range).start === "number" && typeof (value as Range).end === "number";

/** The source ranges of a component's scripts and of every expression in its markup. */
function svelteRanges(source: string): { scripts: Range[]; expressions: Range[] } {
  const root = parse(source, { modern: true }) as unknown as Record<string, unknown>;
  const scripts: Range[] = [];
  for (const key of ["instance", "module"]) {
    const script = root[key] as { content?: Range } | null | undefined;
    if (script?.content && isRange(script.content)) scripts.push({ start: script.content.start, end: script.content.end });
  }
  const expressions = new Map<number, Range>();
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
    for (const key of ["expression", "test"]) {
      const inner = record[key];
      if (isRange(inner) && !templateType.test(String((inner as { type?: unknown }).type))) {
        expressions.set(inner.start, { start: inner.start, end: inner.end });
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
  const parsed = scripts.map((range) =>
    parseSource("component.ts", padding(source.slice(0, range.start)) + source.slice(range.start, range.end)));
  // What the scripts alias is visible to the markup too.
  const aliases: Aliases = { stores: new Set(["repoStore"]), methods: new Set() };
  for (const sf of parsed) collectAliases(sf, names, aliases);
  const mentions = [...aliases.stores, ...aliases.methods];
  for (const range of expressions) {
    const text = source.slice(range.start, range.end);
    if (!mentions.some((name) => text.includes(name))) continue;
    parsed.push(parseSource("component.ts", `${padding(source.slice(0, range.start - 1))}(${text});`));
  }
  for (const sf of parsed) {
    discarded.push(...discardedIn(sf, names, aliases));
    untargeted.push(...untargetedIn(sf));
  }
  return { discarded: [...new Set(discarded)].sort((a, b) => a - b), untargeted: [...new Set(untargeted)].sort((a, b) => a - b) };
}

/** The same scan for a plain TypeScript module. */
export function scanTypeScript(source: string, names: ReadonlySet<string>): { discarded: number[]; untargeted: number[] } {
  const sf = parseSource("module.ts", source);
  return { discarded: discardedIn(sf, names), untargeted: untargetedIn(sf) };
}
