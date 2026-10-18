import { parse } from "svelte/compiler";
import {
  createSourceFile, forEachChild, isArrayBindingPattern, isAsExpression, isBinaryExpression,
  isCallExpression, isCaseClause, isCatchClause, isElementAccessExpression, isIdentifier,
  isNewExpression, isNoSubstitutionTemplateLiteral, isNonNullExpression, isObjectBindingPattern,
  isParenthesizedExpression, isPropertyAccessExpression, isRegularExpressionLiteral,
  isStringLiteral, isSwitchStatement, isTemplateExpression, isVariableDeclaration, ScriptTarget,
  SyntaxKind, type BindingName, type Expression, type Node, type SourceFile,
} from "typescript";

/**
 * Finds the places a backend error is classified by its text.
 *
 * A command rejects with `{ kind, message, detail? }` and `src/lib/ipc/invoke.ts`
 * is the one place that reads it. Anything else that searches an error's text,
 * or compares it with a phrase, breaks silently when a Rust message is
 * reworded or when a caller wraps it.
 *
 * The scan does not trust names. A match is reported when its operand *derives
 * from* an error:
 * - a `catch` binding, or what is destructured from one;
 * - `.message` of anything (an `IpcError`, an `Error`, a payload);
 * - `String(x)` and `formatError(x)`, and what the string methods make of one
 *   (`toLowerCase`, `trim`, `slice`, `replace`, `split` and an index into it,
 *   a cast, a non-null assertion, a template, `??`);
 * - a variable initialised from any of those, anywhere in the same file;
 * and, as a second signal, when the name alone says it is a message
 * (`message`, `error`, `reason`, `stderr`, ...), or when the phrase searched
 * for is one (a sentence, or an upper-case marker like `REPOSITORY_TRUST_REQUIRED`).
 *
 * What it reports: a string search or regex call (`includes`, `startsWith`,
 * `endsWith`, `indexOf`, `lastIndexOf`, `match`, `matchAll`, `test`, `exec`,
 * `search`, `split`, written as a call, as `x["includes"](..)`, or through
 * `.call`), an equality test with a string literal, and a `switch` on one.
 */
export interface ClassifierMatch {
  /** The source line the match starts on, trimmed. */
  line: string;
  /** The match itself, on one line. */
  call: string;
}

const SEARCH = new Set([
  "includes", "startsWith", "endsWith", "indexOf", "lastIndexOf", "match", "matchAll",
  "test", "exec", "search", "split",
]);
/** Methods whose result is still the error's text (or a piece of it). */
const STRING_PRESERVING = new Set([
  "toLowerCase", "toUpperCase", "trim", "trimStart", "trimEnd", "toString", "slice", "substring",
  "substr", "replace", "replaceAll", "normalize", "split", "at", "padStart", "padEnd", "concat",
]);
const MESSAGE_NAMED =
  /^(err|error|e|cause|message|msg|rejection|failure)$|^(err|error)[A-Z]\w*$|(Error|Message|Msg|Failure)$/;
/** A literal that reads like the text of a message: a sentence, or a marker. */
const SENTENCE = /\S\s+\S+\s+\S/;
const MARKER = /^[A-Z][A-Z0-9_]{7,}$/;
const MIN_PHRASE = 12;

function unwrap(node: Node): Node {
  let current = node;
  for (;;) {
    if (isParenthesizedExpression(current) || isNonNullExpression(current) || isAsExpression(current)) {
      current = current.expression;
    } else return current;
  }
}

function literalText(node: Node): string | null {
  const inner = unwrap(node);
  if (isStringLiteral(inner) || isNoSubstitutionTemplateLiteral(inner)) return inner.text;
  if (isRegularExpressionLiteral(inner)) return inner.text;
  return null;
}

function bindingNames(name: BindingName, out: string[], onlyProperty?: string): void {
  if (isIdentifier(name)) {
    out.push(name.text);
  } else if (isObjectBindingPattern(name)) {
    for (const element of name.elements) {
      const property = element.propertyName && isIdentifier(element.propertyName)
        ? element.propertyName.text
        : isIdentifier(element.name) ? element.name.text : undefined;
      if (onlyProperty === undefined || property === onlyProperty) bindingNames(element.name, out);
    }
  } else if (isArrayBindingPattern(name)) {
    for (const element of name.elements) {
      if (!("name" in element)) continue;
      bindingNames(element.name, out, onlyProperty);
    }
  }
}

/** The matches in one `.ts` or `.svelte` file. */
export function classifierMatches(file: string, source: string): ClassifierMatch[] {
  let script = source;
  if (file.endsWith(".svelte")) {
    const component = parse(source, { modern: true, filename: file });
    script = [component.module, component.instance].flatMap((block) => {
      if (!block) return [];
      const content = block.content as unknown as { start: number; end: number };
      return [source.slice(content.start, content.end)];
    }).join("\n");
  }
  const parsed = createSourceFile(file, script, ScriptTarget.Latest, true);

  // Names that hold an error or its text, found to a fixed point so a chain of
  // `const a = String(e); const b = a.trim();` is followed.
  const tainted = new Set<string>();
  const derives = (expression: Expression | Node | undefined): boolean => {
    if (!expression) return false;
    const node = unwrap(expression);
    if (isIdentifier(node)) return tainted.has(node.text) || MESSAGE_NAMED.test(node.text);
    if (isPropertyAccessExpression(node)) {
      return node.name.text === "message" || (MESSAGE_NAMED.test(node.name.text) && derives(node.expression));
    }
    if (isElementAccessExpression(node)) return derives(node.expression);
    if (isTemplateExpression(node)) return node.templateSpans.some((span) => derives(span.expression));
    if (isBinaryExpression(node)) {
      const operator = node.operatorToken.kind;
      if (operator === SyntaxKind.QuestionQuestionToken || operator === SyntaxKind.BarBarToken) {
        return derives(node.left) || derives(node.right);
      }
      if (operator === SyntaxKind.PlusToken) return derives(node.left) || derives(node.right);
      return false;
    }
    if (isCallExpression(node)) {
      const callee = unwrap(node.expression);
      if (isIdentifier(callee) && (callee.text === "String" || callee.text === "formatError")) return true;
      if (isPropertyAccessExpression(callee) && STRING_PRESERVING.has(callee.name.text)) return derives(callee.expression);
    }
    return false;
  };
  const taintVariable = (declaration: Node): boolean => {
    if (!isVariableDeclaration(declaration) || !declaration.initializer) return false;
    const initializer = unwrap(declaration.initializer);
    const names: string[] = [];
    if (isObjectBindingPattern(declaration.name)) {
      // `const { message: m } = e`: only the `message` member is the text.
      bindingNames(declaration.name, names, "message");
      if (derives(initializer)) bindingNames(declaration.name, names);
    } else if (derives(initializer)) {
      bindingNames(declaration.name, names);
    }
    let changed = false;
    for (const name of names) if (!tainted.has(name)) { tainted.add(name); changed = true; }
    return changed;
  };
  const seed = (node: Node): void => {
    if (isCatchClause(node) && node.variableDeclaration) {
      const names: string[] = [];
      bindingNames(node.variableDeclaration.name, names);
      names.forEach((name) => tainted.add(name));
    }
    forEachChild(node, seed);
  };
  seed(parsed);
  for (let pass = 0; pass < 8; pass += 1) {
    let changed = false;
    const walk = (node: Node): void => {
      if (taintVariable(node)) changed = true;
      forEachChild(node, walk);
    };
    walk(parsed);
    if (!changed) break;
  }

  const phrase = (literal: string | null): boolean =>
    literal !== null && literal.length >= MIN_PHRASE && (SENTENCE.test(literal) || MARKER.test(literal));
  const found: ClassifierMatch[] = [];
  const report = (node: Node, owner: SourceFile): void => {
    const { line } = owner.getLineAndCharacterOfPosition(node.getStart());
    found.push({
      line: (script.split("\n")[line] ?? "").trim(),
      call: node.getText(owner).replace(/\s+/g, " "),
    });
  };
  const methodOf = (callee: Node): { method: string; receiver: Node | null } | null => {
    const target = unwrap(callee);
    if (isPropertyAccessExpression(target)) return { method: target.name.text, receiver: target.expression };
    if (isElementAccessExpression(target)) {
      const key = literalText(target.argumentExpression);
      return key === null ? null : { method: key, receiver: target.expression };
    }
    return null;
  };
  /** A string a comparison could be classifying by: not empty (that asks "is there text"). */
  const classifyingLiteral = (node: Node): boolean => {
    const text = literalText(node);
    return text !== null && text !== "" && !isRegularExpressionLiteral(unwrap(node));
  };
  /**
   * `message.replace(phrase, "") === ""` asks "was that all of it", which is a
   * match on the text by another name; a bare `message.trim() === ""` asks only
   * whether there is any.
   */
  const removesFromText = (node: Node): boolean => {
    const inner = unwrap(node);
    if (!isCallExpression(inner)) return false;
    const callee = unwrap(inner.expression);
    return isPropertyAccessExpression(callee)
      && (callee.name.text === "replace" || callee.name.text === "replaceAll")
      && derives(callee.expression);
  };
  const isEmpty = (node: Node): boolean => literalText(node) === "" && !isRegularExpressionLiteral(unwrap(node));
  const equalsLiteral = (left: Node, right: Node): boolean =>
    (classifyingLiteral(right) && derives(left))
    || (classifyingLiteral(left) && derives(right))
    || (isEmpty(right) && removesFromText(left))
    || (isEmpty(left) && removesFromText(right));

  const visit = (node: Node): void => {
    if (isCallExpression(node)) {
      const callee = methodOf(node.expression);
      if (callee) {
        const argument = node.arguments[0];
        const searchesText = SEARCH.has(callee.method)
          && callee.receiver !== null
          && (derives(callee.receiver) || phrase(literalText(argument)));
        // `RE.test(message)`, `new RegExp(..).exec(message)`: the text is the argument.
        const regexOnText = (callee.method === "test" || callee.method === "exec")
          && callee.receiver !== null
          && argument !== undefined
          && (derives(argument)
            // A regex that is itself a sentence is a message pattern, whatever it is applied to.
            || (isRegularExpressionLiteral(unwrap(callee.receiver)) && phrase(literalText(callee.receiver))));
        // `String.prototype.includes.call(message, "...")`.
        const through = callee.receiver === null ? null : unwrap(callee.receiver);
        const viaCall = callee.method === "call"
          && through !== null
          && isPropertyAccessExpression(through)
          && SEARCH.has(through.name.text)
          && argument !== undefined
          && derives(argument);
        // `message.match(re)` and `message.search(re)` are caught by the receiver;
        // `s.match(message)` is not a search of the message.
        if (searchesText || regexOnText || viaCall) report(node, parsed);
      }
    } else if (isNewExpression(node)) {
      // `new RegExp(...)` alone is not a match; its `.test(...)` call is.
    } else if (isBinaryExpression(node)) {
      const operator = node.operatorToken.kind;
      const equality = operator === SyntaxKind.EqualsEqualsEqualsToken
        || operator === SyntaxKind.ExclamationEqualsEqualsToken
        || operator === SyntaxKind.EqualsEqualsToken
        || operator === SyntaxKind.ExclamationEqualsToken;
      if (equality && equalsLiteral(node.left, node.right)) report(node, parsed);
    } else if (isSwitchStatement(node) && derives(node.expression)) {
      const compares = node.caseBlock.clauses.some((clause) => isCaseClause(clause) && literalText(clause.expression) !== null);
      if (compares) report(node, parsed);
    }
    forEachChild(node, visit);
  };
  visit(parsed);
  return found;
}
