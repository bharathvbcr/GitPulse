/**
 * Zero-dependency, high-performance syntax tokenizer for GitPulse IDE file viewer.
 * Tokenizes source code into typed spans for syntax highlighting in both dark and light modes.
 */

export type TokenType =
  | "keyword"
  | "type"
  | "string"
  | "comment"
  | "number"
  | "function"
  | "operator"
  | "tag"
  | "attribute"
  | "property"
  | "punctuation"
  | "regex"
  | "variable"
  | "text";

export interface SyntaxToken {
  text: string;
  type: TokenType;
}

export type SupportedLanguage =
  | "typescript"
  | "javascript"
  | "rust"
  | "svelte"
  | "html"
  | "css"
  | "json"
  | "yaml"
  | "markdown"
  | "python"
  | "go"
  | "shell"
  | "c"
  | "cpp"
  | "sql"
  | "toml"
  | "xml"
  | "diff"
  | "plaintext";

/** Detects language id from file path / extension. */
export function detectLanguageFromPath(filePath: string): SupportedLanguage {
  if (!filePath) return "plaintext";
  const name = filePath.toLowerCase();
  const ext = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1) : "";

  if (ext === "ts" || ext === "tsx" || ext === "mts" || ext === "cts") return "typescript";
  if (ext === "js" || ext === "jsx" || ext === "mjs" || ext === "cjs") return "javascript";
  if (ext === "rs") return "rust";
  if (ext === "svelte") return "svelte";
  if (ext === "html" || ext === "htm") return "html";
  if (ext === "css" || ext === "scss" || ext === "sass" || ext === "less") return "css";
  if (ext === "json" || ext === "jsonc" || ext === "json5") return "json";
  if (ext === "yaml" || ext === "yml") return "yaml";
  if (ext === "md" || ext === "markdown" || ext === "mdx") return "markdown";
  if (ext === "py" || ext === "pyw") return "python";
  if (ext === "go") return "go";
  if (ext === "sh" || ext === "bash" || ext === "zsh" || ext === "fish") return "shell";
  if (ext === "c" || ext === "h") return "c";
  if (ext === "cpp" || ext === "cc" || ext === "cxx" || ext === "hpp" || ext === "hxx") return "cpp";
  if (ext === "sql") return "sql";
  if (ext === "toml" || ext === "ini" || ext === "env" || ext === "conf") return "toml";
  if (ext === "xml" || ext === "svg") return "xml";
  if (ext === "diff" || ext === "patch") return "diff";
  if (name.endsWith("dockerfile") || name.endsWith("makefile") || name.endsWith("justfile")) return "shell";

  return "plaintext";
}

const JS_KEYWORDS = new Set([
  "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger",
  "default", "delete", "do", "else", "enum", "export", "extends", "false", "finally",
  "for", "from", "function", "get", "if", "implements", "import", "in", "instanceof",
  "interface", "let", "new", "null", "of", "package", "private", "protected", "public",
  "return", "set", "static", "super", "switch", "this", "throw", "true", "try", "typeof",
  "undefined", "var", "void", "while", "with", "yield", "type", "as", "is", "declare",
  "module", "namespace", "abstract", "readonly", "override", "satisfies"
]);

const RUST_KEYWORDS = new Set([
  "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
  "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
  "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
  "trait", "true", "type", "unsafe", "use", "where", "while"
]);

const PYTHON_KEYWORDS = new Set([
  "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
  "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
  "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return",
  "True", "try", "while", "with", "yield"
]);

const GO_KEYWORDS = new Set([
  "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough",
  "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range",
  "return", "select", "struct", "switch", "type", "var", "true", "false", "nil"
]);

const SQL_KEYWORDS = new Set([
  "select", "from", "where", "insert", "into", "update", "delete", "create", "drop",
  "table", "alter", "add", "join", "inner", "left", "right", "full", "outer", "on",
  "group", "by", "order", "having", "limit", "offset", "union", "all", "distinct",
  "as", "and", "or", "not", "in", "is", "null", "like", "exists", "between", "case",
  "when", "then", "else", "end", "cast", "primary", "key", "foreign", "references"
]);

const COMMON_TYPES = new Set([
  "string", "number", "boolean", "symbol", "bigint", "any", "unknown", "never", "void",
  "object", "Array", "Record", "Promise", "Set", "Map", "Date", "RegExp", "Function",
  "Error", "Uint8Array", "Int32Array", "Float64Array", "i8", "i16", "i32", "i64", "i128",
  "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "bool", "char",
  "str", "String", "Vec", "Option", "Result", "Box", "Rc", "Arc", "Cell", "RefCell",
  "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64",
  "uintptr", "float32", "float64", "complex64", "complex128", "byte", "rune", "error"
]);

/**
 * Tokenizes a single line of text with language-specific rules.
 */
/**
 * What a line leaves open for the next one.
 *
 * The tokenizer used to start every line from a clean slate, so a construct
 * that spans lines — a `/* … *\/` block, a template literal, a Python
 * docstring, a Go raw string — was coloured on its opening line and then
 * re-read as fresh code on every line after it. That is not a cosmetic
 * shortfall: the contents of a commented-out block were highlighted as live
 * code, which is the opposite of what a reader needs from a comment.
 *
 * `null` means "line starts in ordinary code". Everything else names the
 * delimiter that is still waiting to close, so the next line can resume
 * inside it. The Rust line counter (`analyzer/loc_counter.rs`) carries the
 * same state for the same reason; this is the viewer's half of it.
 */
export type LineCarry =
  | null
  | { kind: "block"; close: string; type: "comment" | "string" };

/** A carry-free start; exported so callers can be explicit about line one. */
export const NO_CARRY: LineCarry = null;

export interface TokenizedLine {
  tokens: SyntaxToken[];
  /** State to hand the NEXT line. */
  carry: LineCarry;
}

/** Delimiters that may stay open across a newline, per language. */
function multilineOpeners(
  language: SupportedLanguage,
): Array<{ open: string; close: string; type: "comment" | "string" }> {
  switch (language) {
    case "python":
      return [
        { open: '"""', close: '"""', type: "string" },
        { open: "'''", close: "'''", type: "string" },
      ];
    case "go":
      // Raw string literals span lines and hide every comment marker inside.
      return [{ open: "`", close: "`", type: "string" }];
    case "typescript":
    case "javascript":
    case "svelte":
      return [{ open: "`", close: "`", type: "string" }];
    default:
      return [];
  }
}

/** Whether this language closes a block comment with the C-style delimiter. */
function hasBlockComments(language: SupportedLanguage): boolean {
  return (
    language !== "python" &&
    language !== "shell" &&
    language !== "yaml" &&
    language !== "toml" &&
    language !== "markdown" &&
    language !== "plaintext" &&
    language !== "diff"
  );
}

/**
 * Resumes a construct the previous line left open.
 *
 * Returns the token covering the resumed run and how much of the line it ate,
 * plus the carry for the following line. A line entirely inside the construct
 * is consumed whole and the carry survives unchanged.
 */
function resumeCarry(
  line: string,
  carry: NonNullable<LineCarry>,
): { token: SyntaxToken; consumed: number; carry: LineCarry } {
  const end = line.indexOf(carry.close);
  if (end === -1) {
    return {
      token: { text: line, type: carry.type },
      consumed: line.length,
      carry,
    };
  }
  const upto = end + carry.close.length;
  return {
    token: { text: line.slice(0, upto), type: carry.type },
    consumed: upto,
    carry: null,
  };
}

/**
 * Tokenizes one line, resuming whatever the previous line left open.
 *
 * `tokenizeLine` remains the carry-free entry point for callers that have a
 * single line and no context (word-diff previews, a fenced snippet's first
 * line); anything rendering a FILE should thread the returned carry, or a
 * block comment will stop being a comment on its second line.
 */
export function tokenizeLineWithCarry(
  line: string,
  language: SupportedLanguage,
  carry: LineCarry = null,
): TokenizedLine {
  if (!line) {
    // A blank line neither opens nor closes anything: the carry passes through
    // untouched. Returning `null` here is what used to end a block comment at
    // the first empty line inside it.
    return { tokens: [], carry };
  }
  if (language === "plaintext") {
    return { tokens: [{ text: line, type: "text" }], carry: null };
  }

  if (language === "diff") {
    if (line.startsWith("+")) return { tokens: [{ text: line, type: "attribute" }], carry: null };
    if (line.startsWith("-")) return { tokens: [{ text: line, type: "operator" }], carry: null };
    if (line.startsWith("@")) return { tokens: [{ text: line, type: "type" }], carry: null };
    return { tokens: [{ text: line, type: "text" }], carry: null };
  }

  if (language === "json") {
    return { tokens: tokenizeJsonLine(line), carry: null };
  }

  const tokens: SyntaxToken[] = [];
  let i = 0;
  const len = line.length;
  let openCarry: LineCarry = null;
  const openers = multilineOpeners(language);

  // Resume first: a line that begins inside a block belongs to that block,
  // whatever its first character would otherwise mean.
  if (carry) {
    const resumed = resumeCarry(line, carry);
    tokens.push(resumed.token);
    i = resumed.consumed;
    if (resumed.carry) return { tokens, carry: resumed.carry };
  }

  while (i < len) {
    // Every token holds at least one character, so a line can never yield more
    // tokens than it has characters. A scan whose entry condition accepts a
    // character its continuation class rejects leaves `i` unmoved — the
    // difference between a mis-coloured line and a hung viewer whose token
    // array eats memory until the process dies, which is what an unhandled
    // '@' did in CSS. Bounded so that failure degrades to a visible artefact.
    if (tokens.length > len) {
      tokens.push({ text: line.slice(i), type: "text" });
      break;
    }
    const char = line[i];

    // Single-line comments
    if (
      (char === "/" && line[i + 1] === "/") ||
      (language === "python" && char === "#") ||
      (language === "shell" && char === "#") ||
      (language === "yaml" && char === "#") ||
      (language === "toml" && char === "#") ||
      (language === "sql" && char === "-" && line[i + 1] === "-")
    ) {
      tokens.push({ text: line.slice(i), type: "comment" });
      break;
    }

    // HTML / XML comment <!-- ... -->
    if (char === "<" && line.slice(i, i + 4) === "<!--") {
      const endIdx = line.indexOf("-->", i + 4);
      if (endIdx !== -1) {
        tokens.push({ text: line.slice(i, endIdx + 3), type: "comment" });
        i = endIdx + 3;
        continue;
      } else {
        tokens.push({ text: line.slice(i), type: "comment" });
        openCarry = { kind: "block", close: "-->", type: "comment" };
        break;
      }
    }

    // Block comments. An unterminated one continues into the next line, which
    // is the whole point of the carry: `/*` on its own used to comment exactly
    // one line and leave the commented-out body highlighted as live code.
    if (char === "/" && line[i + 1] === "*" && hasBlockComments(language)) {
      const endIdx = line.indexOf("*/", i + 2);
      if (endIdx !== -1) {
        tokens.push({ text: line.slice(i, endIdx + 2), type: "comment" });
        i = endIdx + 2;
        continue;
      } else {
        tokens.push({ text: line.slice(i), type: "comment" });
        openCarry = { kind: "block", close: "*/", type: "comment" };
        break;
      }
    }

    // Delimiters that may stay open across a newline are checked before the
    // single-line string scan below, which would otherwise swallow a `"""` as
    // an empty string and leave the docstring's body reading as code.
    if (openers.length > 0) {
      const opener = openers.find((o) => line.startsWith(o.open, i));
      if (opener) {
        const end = line.indexOf(opener.close, i + opener.open.length);
        if (end === -1) {
          tokens.push({ text: line.slice(i), type: opener.type });
          openCarry = { kind: "block", close: opener.close, type: opener.type };
          i = len;
          break;
        }
        const upto = end + opener.close.length;
        tokens.push({ text: line.slice(i, upto), type: opener.type });
        i = upto;
        continue;
      }
    }

    // String literals: "...", '...', `...`
    if (char === '"' || char === "'" || char === "`") {
      const quote = char;
      let j = i + 1;
      let escaped = false;
      while (j < len) {
        if (line[j] === "\\" && !escaped) {
          escaped = true;
          j++;
          continue;
        }
        if (line[j] === quote && !escaped) {
          j++;
          break;
        }
        escaped = false;
        j++;
      }
      tokens.push({ text: line.slice(i, j), type: "string" });
      i = j;
      continue;
    }

    // Numbers: hex (0x...), decimals, floats
    if (/\d/.test(char) || (char === "." && /\d/.test(line[i + 1] || ""))) {
      let j = i;
      if (line.slice(j, j + 2).toLowerCase() === "0x") {
        j += 2;
        while (j < len && /[0-9a-fA-F_]/.test(line[j])) j++;
      } else {
        while (j < len && /[0-9a-zA-Z_.]/.test(line[j])) j++;
      }
      tokens.push({ text: line.slice(i, j), type: "number" });
      i = j;
      continue;
    }

    // HTML / Svelte / XML tags
    if (
      (language === "html" || language === "svelte" || language === "xml") &&
      char === "<" &&
      /[a-zA-Z/!:]/.test(line[i + 1] || "")
    ) {
      let j = i + 1;
      while (j < len && /[a-zA-Z0-9_:\-./]/.test(line[j])) j++;
      tokens.push({ text: line.slice(i, j), type: "tag" });
      i = j;
      continue;
    }

    // Words (identifiers, keywords, types, function names)
    if (/[a-zA-Z_$]/.test(char) || (char === "@" && (language === "svelte" || language === "css"))) {
      // '@' may start a word but is not a word character, so scanning must
      // begin past it. Starting at `i` left `j === i` for an at-rule, giving an
      // empty word and `i = j` — an infinite loop that grew the token array
      // until the process ran out of memory. Any CSS with `@media`, `@import`
      // or `@tailwind` hung the viewer; this repository's own app.css has 34
      // such lines.
      let j = char === "@" ? i + 1 : i;
      while (j < len && /[a-zA-Z0-9_$:-]/.test(line[j])) j++;
      const word = line.slice(i, j);

      // Check next non-whitespace char for function call
      let k = j;
      while (k < len && (line[k] === " " || line[k] === "\t")) k++;
      const isFunction = line[k] === "(" || (language === "rust" && line[k] === "!");

      let type: TokenType = "text";
      const lower = word.toLowerCase();

      if (
        (language === "typescript" || language === "javascript" || language === "svelte") &&
        JS_KEYWORDS.has(word)
      ) {
        type = "keyword";
      } else if (language === "rust" && RUST_KEYWORDS.has(word)) {
        type = "keyword";
      } else if (language === "python" && PYTHON_KEYWORDS.has(word)) {
        type = "keyword";
      } else if (language === "go" && GO_KEYWORDS.has(word)) {
        type = "keyword";
      } else if (language === "sql" && SQL_KEYWORDS.has(lower)) {
        type = "keyword";
      } else if (COMMON_TYPES.has(word) || (/^[A-Z][a-zA-Z0-9_]*$/.test(word) && !isFunction)) {
        type = "type";
      } else if (isFunction) {
        type = "function";
      } else if (word.startsWith("$") && language === "svelte") {
        type = "keyword"; // Svelte 5 runes / stores: $state, $derived, $effect, etc.
      } else if (word.startsWith("@")) {
        type = "attribute";
      } else if ((language === "yaml" || language === "css") && word.endsWith(":")) {
        // The word scanner consumes ':' as a word character, so the colon is
        // already inside `word` and `line[j]` is whatever follows it. Testing
        // line[j] here could never match, which left every YAML key and CSS
        // property rendering as plain text.
        type = "property";
      }

      tokens.push({ text: word, type });
      i = j;
      continue;
    }

    // Punctuation and Operators
    if (/[\{\}\(\)\[\];,.]/.test(char)) {
      tokens.push({ text: char, type: "punctuation" });
      i++;
      continue;
    }

    if (/[+\-*/%=<>!&|^~?:#@]/.test(char)) {
      let j = i;
      while (j < len && /[+\-*/%=<>!&|^~?:#@]/.test(line[j])) j++;
      tokens.push({ text: line.slice(i, j), type: "operator" });
      i = j;
      continue;
    }

    // Whitespace or unknown characters
    tokens.push({ text: char, type: "text" });
    i++;
  }

  return { tokens, carry: openCarry };
}

/**
 * Tokenizes a line with no context.
 *
 * Kept for callers that genuinely have one line and nothing before it. Anything
 * rendering a whole file must thread the carry instead — see
 * `tokenizeLineWithCarry` and `carryPrefix`.
 */
export function tokenizeLine(line: string, language: SupportedLanguage): SyntaxToken[] {
  return tokenizeLineWithCarry(line, language, null).tokens;
}

/**
 * Line-start carries for `lines`, index-aligned, computed once.
 *
 * `result[n]` is the state a renderer must hand line `n`. A virtualized viewer
 * cannot start at the top of the window and be correct — line 4,000 might sit
 * inside a comment opened on line 12 — so the states are computed from the top
 * and cached by the caller against the file's identity.
 *
 * Bounded by construction: one pass, one small object per line, and no token
 * arrays retained.
 */
/**
 * Lazily-extended carry index for a virtualized viewer.
 *
 * A window starting at line 4,000 still needs to know whether line 12 opened a
 * comment, so carries cannot be computed from the top of the WINDOW. Computing
 * them for the whole file up front is equally wrong: that is a full tokenizer
 * pass over an 80,000-line file on every content change, to colour fifty rows.
 *
 * This computes forward only as far as it has been asked, and remembers. Paging
 * to the bottom eventually costs one whole pass — once — and every scroll
 * inside already-visited territory costs nothing.
 */
export function createCarryIndex(
  lines: readonly string[],
  language: SupportedLanguage,
): (index: number) => LineCarry {
  // carries[n] is the state line n STARTS in; line 0 starts clean.
  const carries: LineCarry[] = [null];
  let computedTo = 0;
  return (index: number): LineCarry => {
    if (index <= 0) return null;
    const target = Math.min(index, lines.length);
    while (computedTo < target) {
      const carry = tokenizeLineWithCarry(
        lines[computedTo] ?? "",
        language,
        carries[computedTo] ?? null,
      ).carry;
      computedTo += 1;
      carries[computedTo] = carry;
    }
    return carries[target] ?? null;
  };
}

export function carryPrefix(
  lines: readonly string[],
  language: SupportedLanguage,
): LineCarry[] {
  const carries: LineCarry[] = new Array(lines.length);
  let carry: LineCarry = null;
  for (let index = 0; index < lines.length; index += 1) {
    carries[index] = carry;
    carry = tokenizeLineWithCarry(lines[index] ?? "", language, carry).carry;
  }
  return carries;
}

/** Specialized tokenizer for JSON lines. */
function tokenizeJsonLine(line: string): SyntaxToken[] {
  const tokens: SyntaxToken[] = [];
  let i = 0;
  const len = line.length;

  while (i < len) {
    // Every token holds at least one character, so a line can never yield more
    // tokens than it has characters. A scan whose entry condition accepts a
    // character its continuation class rejects leaves `i` unmoved — the
    // difference between a mis-coloured line and a hung viewer whose token
    // array eats memory until the process dies, which is what an unhandled
    // '@' did in CSS. Bounded so that failure degrades to a visible artefact.
    if (tokens.length > len) {
      tokens.push({ text: line.slice(i), type: "text" });
      break;
    }
    const char = line[i];

    if (char === '"') {
      let j = i + 1;
      let escaped = false;
      while (j < len) {
        if (line[j] === "\\" && !escaped) {
          escaped = true;
          j++;
          continue;
        }
        if (line[j] === '"' && !escaped) {
          j++;
          break;
        }
        escaped = false;
        j++;
      }
      const str = line.slice(i, j);
      // Check if followed by colon (property key)
      let k = j;
      while (k < len && (line[k] === " " || line[k] === "\t")) k++;
      const isKey = line[k] === ":";
      tokens.push({ text: str, type: isKey ? "property" : "string" });
      i = j;
      continue;
    }

    if (/\d/.test(char) || (char === "-" && /\d/.test(line[i + 1] || ""))) {
      let j = i;
      while (j < len && /[0-9eE.+\-_]/.test(line[j])) j++;
      tokens.push({ text: line.slice(i, j), type: "number" });
      i = j;
      continue;
    }

    if (/[a-zA-Z]/.test(char)) {
      let j = i;
      while (j < len && /[a-zA-Z]/.test(line[j])) j++;
      const word = line.slice(i, j);
      const isKeyword = word === "true" || word === "false" || word === "null";
      tokens.push({ text: word, type: isKeyword ? "keyword" : "text" });
      i = j;
      continue;
    }

    if (/[\{\}\[\]:,]/.test(char)) {
      tokens.push({ text: char, type: "punctuation" });
      i++;
      continue;
    }

    tokens.push({ text: char, type: "text" });
    i++;
  }

  return tokens;
}

/** CSS color class corresponding to token type. */
export function tokenClass(type: TokenType): string {
  switch (type) {
    case "keyword":
      return "text-purple-400 font-medium";
    case "type":
      return "text-cyan-400";
    case "string":
      return "text-emerald-400";
    case "comment":
      return "text-textMuted/70 italic";
    case "number":
      return "text-amber-400";
    case "function":
      return "text-blue-400";
    case "operator":
      return "text-rose-400";
    case "tag":
      return "text-rose-400 font-medium";
    case "attribute":
      return "text-amber-300";
    case "property":
      return "text-sky-300";
    case "punctuation":
      return "text-textMuted";
    case "regex":
      return "text-rose-300";
    case "variable":
      return "text-indigo-300";
    case "text":
    default:
      return "text-textPrimary/90";
  }
}
