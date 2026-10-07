use regex::Regex;
use std::sync::OnceLock;

use crate::model::*;

/// The method a route records when its source names no single verb.
///
/// A Flask `@app.route(...)` with no `methods=` accepts GET *and* HEAD, and a
/// `methods=` this pass cannot read as literals names nothing it can see.
/// Both are "the source does not say", which is a different claim from "GET" —
/// and the one downstream verb matching already understands as a wildcard.
const UNSPECIFIED_METHOD: &str = "ANY";

/// A Python route decorator: any receiver, and a path the framework would accept.
///
/// The receiver used to be an allow-list of three names — `app`, `router`,
/// `api` — which is not how either framework is written. A Flask blueprint is
/// `bp = Blueprint(...)` and a FastAPI router is `users = APIRouter()`, so the
/// receiver is whatever the author named the object, and `@bp.route("/users")`
/// extracted nothing at all. A module holding only blueprint routes answered
/// `count: 0` over a scan reporting `complete: true`.
///
/// Precision moves from the receiver's *name* to the path's *shape*, which is a
/// property of the frameworks rather than a guess about how authors name
/// things: Werkzeug's `Rule.__init__` raises `ValueError` unless the rule
/// starts with `/`, and Starlette's `Route.__init__` asserts
/// `path.startswith("/")`. A decorator whose first argument is a string literal
/// not starting with `/` is therefore not a route in either framework. That one
/// test is what keeps `@mock.patch("os.path.exists")` out of the route table:
/// it has an identifier receiver, an HTTP-verb attribute, a string literal and
/// a decorated `def`, so every *other* signal admits it.
///
/// Anchored to the start of a line because a Python decorator can begin nowhere
/// else — which also stops a commented-out `# @app.route("/x")` from binding a
/// handler that nothing reaches any more.
fn python_route_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)^[ \t]*@(?:[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\.(get|post|put|delete|patch|options|head|route)\s*\(\s*["'](/[^"']*)["']"#,
        )
        .map_err(|error| format!("invalid Python route matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Django's URLconf entries: `path()` and `re_path()` in a `urlpatterns` list.
///
/// Unlike every other matcher here the route is a plain call, not a decorator
/// or a method on an app object, so there is no `@` or `app.` prefix to key on.
/// `path` on its own is an ordinary identifier, hence the leading `[^\w.]`:
/// `os.path(` and `mypath(` are not routes. The pattern literal may be empty —
/// `path("", views.index)` is how an app names its own root — which is why this
/// alone of the matchers accepts a zero-length path.
fn django_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?m)(?:^|[^\w.])(re_path|path)\s*\(\s*[rbuRBU]{0,2}["']([^"']*)["']"#)
            .map_err(|error| format!("invalid Django route matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn axum_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\.route\s*\(\s*["']([^"']+)["']\s*,\s*(get|post|put|delete|patch)\s*\(\s*([A-Za-z0-9_]+)\s*\)\s*\)"#,
        )
        .map_err(|error| format!("invalid Axum route matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// An Express route: any receiver, a path, and a handler argument after it.
///
/// `(app|router)` missed every router bound to another name —
/// `const api = express.Router()`, `const v1 = Router()`, `this.router` — and
/// the widening the Python matcher gets applies here for the same reason.
///
/// JavaScript then needs a second guard that Python does not, because `X.get`
/// with a string argument is one of the most common shapes in the language:
/// `axios.get('/api/users')`, `redis.get('key')`, `cache.get(k)`. Two
/// properties separate a route from all of those. The path starts with `/`, or
/// is the `*` catch-all Express documents. And the call has a *second*
/// argument, because a route without a handler is not a route.
///
/// That second test also closes a false positive the old `app` receiver already
/// had: `app.get(name)` with one argument is Express's settings *getter*, so
/// `app.get('view engine')` was recorded as a `GET view engine` route carrying
/// no handler — verified against the pre-change matcher.
///
/// The receiver is captured, not discarded, because two things downstream need
/// it: `non_router_receivers` asks whether this file bound it to a package, and
/// nothing else in the match can answer that. What it is *not* used for is a
/// list of acceptable names — that is the defect this matcher exists to undo.
fn express_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)\b([A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*)\.(get|post|put|delete|patch)\s*\(\s*["'](\*|/[^"']*)["']\s*,"#,
        )
        .map_err(|error| format!("invalid Express route matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Advance string-literal state by one character, reporting whether that
/// character is inside a literal and so is text rather than structure.
///
/// Every scan over an argument list needs this: a bracket, a paren or a comma
/// inside a string is not a nesting change or a separator, and reading one as
/// structure ends a scan in the middle of an argument. A quote left open at end
/// of line closes there — a lone apostrophe in a comment or a regex literal
/// would otherwise swallow the rest of the file, and losing every later route
/// is a worse answer than misreading one line.
fn in_string_literal(character: char, quote: &mut Option<char>, escaped: &mut bool) -> bool {
    match *quote {
        Some(open) => {
            if *escaped {
                *escaped = false;
            } else if character == '\\' {
                *escaped = true;
            } else if character == open || character == '\n' {
                *quote = None;
            }
            true
        }
        None if character == '\'' || character == '"' => {
            *quote = Some(character);
            *escaped = false;
            true
        }
        None => false,
    }
}

/// How far past a call's opening paren its closing paren is looked for.
///
/// The scan runs once per route match, and a source that never closes the call
/// — an unbalanced paren, which a tolerant grammar still hands over — would
/// otherwise send every match to end of file, making extraction quadratic in
/// the size of the file. No real argument list comes near this, so the cap
/// costs nothing that exists and bounds the pathological case.
const MAX_CALL_SCAN: usize = 64 * 1024;

/// The rest of a call's argument list, and where the call ends.
///
/// `after` is a position *inside* the arguments — the end of a regex match that
/// already consumed the opening paren and the first argument — so the text
/// returned is the remainder of the list. The second element is the byte index
/// just past the `)` that closes the call, which is where whatever follows the
/// call begins.
///
/// Nesting is tracked so an inner call, list or object does not end the scan
/// early, and `in_string_literal` keeps quoted text out of that structure. A
/// call whose closing paren is not found within `MAX_CALL_SCAN` reads as
/// unfinished.
fn call_arguments(source: &str, after: usize) -> Option<(&str, usize)> {
    let rest = source.get(after..)?;
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (offset, character) in rest.char_indices() {
        if offset >= MAX_CALL_SCAN {
            return None;
        }
        if in_string_literal(character, &mut quote, &mut escaped) {
            continue;
        }
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => return Some((&rest[..offset], after + offset + 1)),
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The value text of one keyword argument the call itself passes.
///
/// Only the call's own keywords count. A `methods` key nested inside another
/// argument (`defaults={"methods": ...}`) belongs to that argument, one inside
/// a string literal is text, and `allowed_methods=` is a different keyword —
/// reading any of them would put a route on the graph under a verb the app
/// never registered. `==` is a comparison, not a binding.
fn keyword_argument<'a>(arguments: &'a str, name: &str) -> Option<&'a str> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut previous: Option<char> = None;
    for (offset, character) in arguments.char_indices() {
        if in_string_literal(character, &mut quote, &mut escaped) {
            previous = Some(character);
            continue;
        }
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ if depth == 0
                && !matches!(previous, Some(before) if is_identifier_char(before))
                && arguments[offset..].starts_with(name) =>
            {
                let tail = arguments[offset + name.len()..].trim_start();
                if let Some(value) = tail.strip_prefix('=') {
                    if !value.starts_with('=') {
                        return Some(value.trim_start());
                    }
                }
            }
            _ => {}
        }
        previous = Some(character);
    }
    None
}

/// The HTTP verbs a literal `methods=` list or tuple declares.
///
/// Only string literals inside a bracketed literal are read, and only when that
/// literal is the whole value. Anything else — a name, a call, a comprehension,
/// a list that is one operand of a larger expression, a literal the source
/// never closes — is a verb set this pass cannot see. Returning the part it
/// could read would state a smaller verb set than the app registers, so it
/// returns nothing, which is what tells the caller to record the route as
/// unspecified.
fn declared_methods(value: &str) -> Vec<String> {
    if !matches!(value.chars().next(), Some('[') | Some('(')) {
        return Vec::new();
    }
    let mut verbs: Vec<String> = Vec::new();
    let mut literal = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut end = None;
    for (offset, character) in value.char_indices() {
        if let Some(open) = quote {
            if character == open {
                let verb = literal.trim().to_uppercase();
                if !verb.is_empty() && !verbs.contains(&verb) {
                    verbs.push(verb);
                }
                literal.clear();
                quote = None;
            } else {
                literal.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(offset + character.len_utf8());
                    break;
                }
            }
            _ => {}
        }
    }
    // The literal must be the whole value: what follows it is either nothing or
    // the next keyword argument. An `if`, `or` or `+` after it means the verb
    // set is computed and this literal is only one branch of it.
    match end.map(|end| value[end..].trim_start()) {
        Some(rest) if rest.is_empty() || rest.starts_with(',') => verbs,
        _ => Vec::new(),
    }
}

/// The verbs one Python route decorator registers.
///
/// A verb decorator — `@app.get`, `@router.post` — is its own answer. Flask's
/// classic `@app.route` is not: its verbs live in `methods=`, and without that
/// argument the rule accepts GET and HEAD, so the route is recorded as
/// unspecified rather than as one invented verb. Several declared verbs are
/// several routes, because the verb is half a route's identity downstream: the
/// resolver keys its edge on `"{method} {path}"`, and the API-route consumer
/// matches a client call's verb against it, so a single `"GET,POST"` would
/// match neither.
fn python_route_methods(verb: &str, arguments: Option<&str>) -> Vec<String> {
    if !verb.eq_ignore_ascii_case("route") {
        return vec![verb.to_uppercase()];
    }
    let declared = arguments
        .and_then(|arguments| keyword_argument(arguments, "methods"))
        .map(declared_methods)
        .unwrap_or_default();
    if declared.is_empty() {
        return vec![UNSPECIFIED_METHOD.to_string()];
    }
    declared
}

/// Name of the definition a Python route decorator is attached to.
///
/// `@app.get("/items")` carries no handler name of its own — the handler is the
/// definition the decorator is applied to, which may sit several stacked
/// decorators later. Scanning forward for it is what makes a FastAPI or Flask
/// route resolvable at all; without it the route names nothing and its handler
/// has no incoming edge, so an endpoint reachable only over HTTP reads as dead.
///
/// The three prefixes below are the whole of what a decorator may target:
/// Python's grammar is `decorated: decorators (classdef | funcdef |
/// async_funcdef)`. `class` belongs here because a class-based view — Flask's
/// `MethodView`, or any callable class — is as much a handler as a `def`, and
/// reading only the two function forms made the third resolve to `""`, which
/// `resolver.rs` cannot tell from an Express arrow function that has no name by
/// design and so skips without a ledger entry.
///
/// Returns `None` when the decorator is not attached to a definition, rather
/// than guessing: a route bound to the wrong symbol is worse than one bound to
/// none.
fn python_decorated_handler(source: &str, after: usize) -> Option<String> {
    for line in source.get(after..)?.lines().skip(1) {
        let trimmed = line.trim();
        // Blank lines, comments and further stacked decorators sit between the
        // route decorator and its definition.
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('@') {
            continue;
        }
        let definition = trimmed
            .strip_prefix("async def ")
            .or_else(|| trimmed.strip_prefix("def "))
            .or_else(|| trimmed.strip_prefix("class "))?;
        let name: String = definition
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .collect();
        return (!name.is_empty()).then_some(name);
    }
    None
}

/// Whether a Python file is a Django URLconf.
///
/// `path(...)` is too ordinary a call to read as a route on its name alone, and
/// a route bound to the wrong symbol is worse than no route at all:
/// `HandlesRoute` is what tells liveness a handler is reached from outside the
/// call graph, so a false one silences a genuinely dead symbol. Django's own
/// contract supplies the discriminator — `include()` resolves a URLconf module
/// by looking up `urlpatterns` in it, so every URLconf defines that name — and
/// the import covers the rare pattern list built under another name. Over 9,030
/// third-party Python files one file matched `path(<literal>` and none of them
/// was a URLconf, so the gate turns away a class of false routes without
/// costing a real one.
fn is_django_urlconf(source: &str) -> bool {
    source.contains("urlpatterns")
        || source.contains("django.urls")
        || source.contains("django.conf.urls")
}

/// The first positional argument a call passes after the one already consumed.
///
/// `arguments` is what `call_arguments` returns for a match that ended on the
/// route pattern, so it opens on the comma separating that pattern from the
/// view. Nesting and string literals are tracked because a Django view is
/// routinely written as a call — `include(...)`, `Cls.as_view()` — whose own
/// commas and parens must not end the argument early.
///
/// A keyword does not count as positional. `path("x/", name="home")` passes no
/// view, and reading `name=` as one would bind the route to whatever symbol
/// happened to be called `home`.
///
/// Comments are skipped, because a URLconf entry spread over several lines is
/// routinely annotated: a comma in prose would otherwise end the argument
/// before the view, and an apostrophe would open a string literal that swallows
/// the rest of the line.
fn first_positional_argument(arguments: &str) -> Option<&str> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut commented = false;
    let mut start: Option<usize> = None;
    let mut end = arguments.len();
    for (offset, character) in arguments.char_indices() {
        // Checked before the string state advances: a quote inside a comment is
        // prose, not the start of a literal.
        if commented {
            commented = character != '\n';
            continue;
        }
        if in_string_literal(character, &mut quote, &mut escaped) {
            continue;
        }
        if character == '#' {
            commented = true;
            continue;
        }
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => match start {
                None => start = Some(offset + character.len_utf8()),
                Some(_) => {
                    end = offset;
                    break;
                }
            },
            _ => {}
        }
    }
    let candidate = arguments.get(start?..end)?.trim();
    let keyword = candidate.find('=').is_some_and(|split| {
        let (key, value) = candidate.split_at(split);
        !key.is_empty() && key.chars().all(is_identifier_char) && !value.starts_with("==")
    });
    (!candidate.is_empty() && !keyword).then_some(candidate)
}

/// Whether a URLconf entry mounts another URLconf instead of naming a view.
///
/// `path("api/", include("app.urls"))` nests a whole URLconf under a prefix.
/// Following it means reading the included module and prefixing every path it
/// declares, which this pass does not do, so the entry is skipped rather than
/// recorded: a route with no handler would put a path on the graph that nothing
/// serves, and taking `include` for the view would bind the route to `include`
/// itself.
///
/// The cost of skipping is that a path inside an included URLconf is recorded
/// as its own file declares it, without the prefix its parent mounts it under.
/// That is the honest half of the answer — the alternative is a path the app
/// does not serve.
fn mounts_included_urlconf(target: &str) -> bool {
    target
        .split('(')
        .next()
        .and_then(|head| head.trim().rsplit('.').next())
        .is_some_and(|name| name.trim() == "include")
}

/// The symbol name a URLconf entry's view argument refers to.
///
/// `views.user_detail` is a member expression whose final segment is the name
/// the symbol index knows — the same shape an Express handler argument takes. A
/// class-based view is registered as `UserDetail.as_view()`, where the class is
/// the symbol and `as_view` is Django's accessor for it; the resolver binds a
/// route to a class as readily as to a function.
///
/// Anything else — a lambda, a decorator call wrapping the view — has no name
/// this pass can read, and yields `None` rather than a fragment of one.
fn view_symbol_name(target: &str) -> Option<String> {
    let bare = match target.split_once(".as_view") {
        Some((head, tail)) if tail.trim_start().starts_with('(') => head,
        _ => target,
    };
    let bare = bare.rsplit('.').next()?.trim();
    let name: String = bare
        .chars()
        .take_while(|character| is_identifier_char(*character))
        .collect();
    (!name.is_empty() && name.len() == bare.len()).then_some(name)
}

/// The index of the `)` closing the regex group that opens at `start`.
///
/// Escapes and character classes are honoured: `\(` is a literal paren and a
/// `(` inside `[...]` is an ordinary character, so neither opens a group. A
/// group the pattern never closes yields `None`.
fn regex_group_end(characters: &[char], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_class = false;
    let mut index = start;
    while index < characters.len() {
        match characters[index] {
            '\\' => index += 1,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// One regex group rewritten as a path-template segment.
///
/// `inner` is the group's body, without its own parentheses.
fn regex_group_template(inner: &[char]) -> String {
    let text: String = inner.iter().collect();
    // `(?P<name>...)` is Django's named capture, and the name is the parameter.
    if let Some(rest) = text.strip_prefix("?P<") {
        if let Some(close) = rest.find('>') {
            let name = &rest[..close];
            if !name.is_empty() && name.chars().all(is_identifier_char) {
                return format!("<{name}>");
            }
        }
    }
    // `(?...)` captures nothing — a lookaround, an inline flag, a grouped
    // alternation. None of them is a parameter, so the group stays as written.
    if text.starts_with('?') {
        return format!("({text})");
    }
    // A plain capturing group is a parameter Django passes to the view by
    // position, so it has no name to carry.
    "<arg>".to_string()
}

/// A `re_path()` regex rewritten as the path template `path()` would spell.
///
/// `re_path` patterns are regexes, not templates: `r"^legacy/(?P<pk>\d+)/$"`
/// describes the route `path("legacy/<pk>/")` describes, but nothing
/// downstream reads regex syntax — the route normalizer substitutes `<name>`
/// and would leave `(?P<pk>\d+)` as three unmatchable path segments. Anchors
/// are dropped, a named group becomes its `<name>`, and an unnamed capturing
/// group becomes `<arg>`; the normalizer collapses either to the same wildcard.
///
/// Constructs with no template equivalent — a non-capturing group, an
/// alternation, a bare `\d+` outside any group — are carried through verbatim.
/// A partly rewritten path still says what the source says, and says it in the
/// segments that matter; refusing to rewrite would drop the route entirely, and
/// inventing a template for a pattern this pass cannot read would claim a shape
/// the app does not serve.
fn django_regex_to_template(pattern: &str) -> String {
    let body = pattern.strip_prefix('^').unwrap_or(pattern);
    let body = body
        .strip_suffix('$')
        .or_else(|| body.strip_suffix("\\Z"))
        .unwrap_or(body);

    let characters: Vec<char> = body.chars().collect();
    let mut template = String::new();
    let mut index = 0;
    while index < characters.len() {
        match characters[index] {
            // An escape makes the next character literal. Punctuation is
            // carried as itself — `\.` is a dot in the path — while an escaped
            // letter is a character class (`\d`, `\w`) and stays as written.
            '\\' if index + 1 < characters.len() => {
                if characters[index + 1].is_alphanumeric() {
                    template.push('\\');
                }
                template.push(characters[index + 1]);
                index += 2;
            }
            '(' => match regex_group_end(&characters, index) {
                Some(end) => {
                    template.push_str(&regex_group_template(&characters[index + 1..end]));
                    index = end + 1;
                }
                // A group the pattern never closes is not structure this pass
                // can read, so the rest is carried over as written.
                None => {
                    template.extend(&characters[index..]);
                    break;
                }
            },
            character => {
                template.push(character);
                index += 1;
            }
        }
    }
    template
}

/// Handler name from an Express route's argument list.
///
/// `app.get("/x", handleUsers)` names its handler in the final argument, after
/// any middleware. The argument may be an identifier, a member expression, or a
/// function expression — named or anonymous. An anonymous handler genuinely has
/// no name, and yields `None` rather than a placeholder: a placeholder would
/// resolve to any symbol that happened to share it.
/// Names this file binds to a third-party package, which is not a router.
///
/// The shape test on the final argument settles `axios.get(url, {headers})`,
/// but not `axios.get(url, config)` — an identifier config is shaped exactly
/// like a handler. The receiver is what separates them, and the file says what
/// the receiver is without any need to resolve across modules.
///
/// An Express router is *constructed* — `express()`, `express.Router()`,
/// `Router()` — never imported ready-made from a package. So a receiver this
/// file binds directly to a **bare** specifier other than express is not a
/// router: `require('axios')`, `import got from 'got'`,
/// `import * as ky from 'ky'`. A relative specifier (`./routes/users`) is left
/// alone, because a local module genuinely can export a router.
///
/// This is a deny-list of *bindings read out of this file*, not a list of names
/// anyone guessed. That distinction is the whole point: an allow-list of
/// receiver names is what produced the defect this pass exists to fix, and it
/// failed on every name nobody thought of. A binding cannot be missing from a
/// list of things the author wrote — at worst the author wrote nothing here,
/// and an unknown receiver stays a candidate rather than being turned away.
fn non_router_receivers(source: &str) -> std::collections::HashSet<String> {
    let mut bound = std::collections::HashSet::new();
    let Ok(imports) = package_binding_re() else {
        return bound;
    };
    for capture in imports.captures_iter(source) {
        // The require arm and the import arm each carry their own specifier
        // group, because one regex cannot name the same group twice.
        let Some(specifier) = capture
            .name("from")
            .or_else(|| capture.name("from2"))
            .map(|m| m.as_str())
        else {
            continue;
        };
        // A relative or absolute path may export a router; express itself and
        // its own subpaths are the router's source, not a rival to it.
        if specifier.starts_with('.')
            || specifier.starts_with('/')
            || specifier == "express"
            || specifier.starts_with("express/")
        {
            continue;
        }
        for group in ["default", "namespace", "required"] {
            if let Some(name) = capture.name(group) {
                bound.insert(name.as_str().to_string());
            }
        }
        if let Some(named) = capture.name("named") {
            for entry in named.as_str().split(',') {
                // `{ get as httpGet }` binds the alias, which is the name a
                // call site would use.
                let binding = entry.rsplit(" as ").next().unwrap_or(entry).trim();
                if !binding.is_empty() && binding.chars().all(is_binding_char) {
                    bound.insert(binding.to_string());
                }
            }
        }
    }

    // One hop of propagation, for the client-factory idiom: `axios.create()`
    // returns a client, so whatever it was assigned to is no more a router than
    // `axios` is. One hop is enough for every form of this in the wild, and
    // stopping there keeps the scan linear.
    if let Ok(factory) = factory_binding_re() {
        for capture in factory.captures_iter(source) {
            let (Some(name), Some(source_name)) = (capture.get(1), capture.get(2)) else {
                continue;
            };
            if bound.contains(source_name.as_str()) {
                bound.insert(name.as_str().to_string());
            }
        }
    }
    bound
}

fn is_binding_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

/// `const X = require("pkg")` and the three ESM import forms, as whole bindings.
///
/// The require arm is anchored to end of statement so `require("express")
/// .Router()` is not read as a bare package binding: that expression
/// *constructs* a router, and only the un-suffixed form binds the module
/// itself.
fn package_binding_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)^[ \t]*(?:(?:const|let|var)[ \t]+(?P<required>[A-Za-z_$][A-Za-z0-9_$]*)[ \t]*=[ \t]*require[ \t]*\([ \t]*["'](?P<from>[^"']+)["'][ \t]*\)[ \t]*;?[ \t]*$|import[ \t]+(?:(?P<default>[A-Za-z_$][A-Za-z0-9_$]*)|\*[ \t]+as[ \t]+(?P<namespace>[A-Za-z_$][A-Za-z0-9_$]*)|\{(?P<named>[^}]*)\})[ \t]+from[ \t]*["'](?P<from2>[^"']+)["'])"#,
        )
        .map_err(|error| format!("invalid package binding matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// `const client = axios.create(...)` — a binding whose value comes from a call
/// on another binding.
fn factory_binding_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)^[ \t]*(?:const|let|var)[ \t]+([A-Za-z_$][A-Za-z0-9_$]*)[ \t]*=[ \t]*([A-Za-z_$][A-Za-z0-9_$]*)(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*[ \t]*\("#,
        )
        .map_err(|error| format!("invalid factory binding matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// The final top-level argument of a call, trimmed.
///
/// One owner for a question two callers ask of the same text: which argument is
/// the handler, and is it shaped like one at all. Nesting and string literals
/// are tracked so a comma inside an arrow body, a nested call, an array of
/// middleware, an options object or a quoted string is not read as a separator.
///
/// An argument list with no top-level comma yields the whole list, which is the
/// right answer: a call with one argument has that argument as its last.
fn last_top_level_argument(arguments: &str) -> &str {
    top_level_arguments(arguments)
        .last()
        .map_or("", |(_, argument)| *argument)
}

/// Every top-level argument of a call, trimmed, each with the byte offset in
/// `arguments` where its trimmed text starts.
///
/// The one splitter: [`last_top_level_argument`] is its final element, and the
/// middleware producers read the elements before it. Always yields at least one
/// entry — an empty list is one empty argument, which is what the handler
/// check needs to see to reject a settings getter.
fn top_level_arguments(arguments: &str) -> Vec<(usize, &str)> {
    fn trimmed(text: &str, start: usize, end: usize) -> (usize, &str) {
        let raw = &text[start..end];
        let lead = raw.len() - raw.trim_start().len();
        (start + lead, raw.trim())
    }
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut start = 0usize;
    let mut out = Vec::new();
    for (offset, character) in arguments.char_indices() {
        if in_string_literal(character, &mut quote, &mut escaped) {
            continue;
        }
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(trimmed(arguments, start, offset));
                start = offset + 1;
            }
            _ => {}
        }
    }
    out.push(trimmed(arguments, start, arguments.len()));
    out
}

/// Whether a call's final argument is shaped like an Express handler.
///
/// `app.METHOD(path, ...callbacks)` is Express's whole signature: every
/// argument after the path is a callback, and the framework has no form that
/// takes trailing options. An HTTP client's `get` is the opposite shape —
/// `axios.get(url, config)` — so the final argument is the one place the two
/// differ structurally, whatever the receiver is called.
///
/// Data is rejected: an object literal (`{headers}`, `{params: {...}}` — the
/// axios config), a string, a template literal, a number, a boolean, `null` and
/// `undefined`. Everything callable is accepted: an identifier, a member
/// expression, a function expression, an arrow function, a call returning a
/// handler (`asyncHandler(getUsers)` — the wrapper idiom), and an array literal,
/// because Express documents `app.get(path, [mw1, mw2])`.
///
/// An empty argument is not a handler: that is the settings getter,
/// `app.get('view engine')`, whose argument list ends at the path.
fn express_final_argument_is_a_handler(candidate: &str) -> bool {
    let Some(first) = candidate.chars().next() else {
        return false;
    };
    // An object literal is data. `{` opening an arrow body cannot appear here:
    // an arrow's `{` always follows its `=>`, never starts the argument.
    if first == '{' || first == '"' || first == '\'' || first == '`' {
        return false;
    }
    if first.is_ascii_digit() || (first == '-' && candidate.len() > 1) {
        return false;
    }
    !matches!(candidate, "null" | "undefined" | "true" | "false")
}

fn express_handler_name(source: &str, after: usize) -> Option<String> {
    let (arguments, _) = call_arguments(source, after)?;
    let candidate = last_top_level_argument(arguments);

    // A named function expression names the handler.
    if let Some(tail) = candidate.strip_prefix("function") {
        let name: String = tail
            .trim_start()
            .chars()
            .take_while(|character| character.is_alphanumeric() || *character == '_')
            .collect();
        return (!name.is_empty()).then_some(name);
    }

    // An identifier or member expression: the final segment is the name the
    // symbol index knows.
    let bare = candidate.rsplit('.').next()?.trim();
    let name: String = bare
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    if name.is_empty() || name.len() != bare.len() {
        // Anything else — an arrow function, a call, an object literal — has no
        // name to record.
        return None;
    }
    Some(name)
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Words that open an expression but name no symbol: a `function`/`func`
/// literal with no name, an `async` arrow, a constructor call, a literal.
const NOT_A_SYMBOL: &[&str] = &[
    "function",
    "func",
    "async",
    "await",
    "new",
    "typeof",
    "true",
    "false",
    "null",
    "undefined",
    "nil",
    "this",
];

/// The symbol a middleware argument names, and the binding it was reached
/// through.
///
/// `auth` → `("auth", None)`; `middleware.Logger` → `("Logger",
/// Some("middleware"))`; `requireRole('admin')` → `("requireRole", None)`,
/// because the factory is the symbol the repository declares and the
/// middleware is what it returns; `function auth(req, res, next) {…}` →
/// `("auth", None)`. Anything else — an arrow function, a `func` literal, an
/// object, a string — names nothing, and yields an empty name so the entry is
/// still listed by its expression rather than dropped.
fn middleware_reference(expression: &str) -> (String, Option<String>) {
    let text = expression.trim();
    if let Some(tail) = text.strip_prefix("function") {
        let name: String = tail
            .trim_start()
            .chars()
            .take_while(|character| is_binding_char(*character))
            .collect();
        return (name, None);
    }
    // A call is named by its callee: everything before the first `(`.
    let callee = text.find('(').map_or(text, |at| &text[..at]).trim();
    let segments: Vec<&str> = callee.split('.').collect();
    let well_formed = segments.iter().all(|segment| {
        !segment.is_empty()
            && segment.chars().all(is_binding_char)
            && !segment.starts_with(|c: char| c.is_ascii_digit())
    });
    let Some(last) = segments.last().filter(|_| well_formed) else {
        return (String::new(), None);
    };
    if NOT_A_SYMBOL.contains(last) {
        return (String::new(), None);
    }
    let qualifier = (segments.len() > 1).then(|| segments[0].to_string());
    (last.to_string(), qualifier)
}

/// One middleware entry, from an argument that starts `offset` bytes into
/// `source`.
fn middleware_entry(argument: &str, offset: usize, scope: MiddlewareScope) -> ExtractedMiddleware {
    let (name, qualifier) = middleware_reference(argument);
    let mut expression = argument.to_string();
    if expression.len() > MIDDLEWARE_EXPRESSION_CAP {
        let mut cut = MIDDLEWARE_EXPRESSION_CAP;
        while !expression.is_char_boundary(cut) {
            cut -= 1;
        }
        expression.truncate(cut);
        expression.push('…');
    }
    ExtractedMiddleware {
        name,
        qualifier,
        expression,
        scope,
        span: Span {
            start_byte: offset,
            end_byte: offset + argument.len(),
        },
    }
}

/// Middleware entries for a run of arguments, an array literal flattened in
/// place — Express documents `app.get(path, [mw1, mw2], handler)`.
///
/// `base` is where `arguments` starts in the file, so every span is absolute.
fn middleware_entries(
    arguments: &[(usize, &str)],
    base: usize,
    scope: MiddlewareScope,
) -> Vec<ExtractedMiddleware> {
    let mut entries = Vec::new();
    for (offset, argument) in arguments {
        if argument.is_empty() {
            continue;
        }
        if let Some(inner) = argument
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            for (inner_offset, element) in top_level_arguments(inner) {
                if !element.is_empty() {
                    entries.push(middleware_entry(
                        element,
                        base + offset + 1 + inner_offset,
                        scope,
                    ));
                }
            }
            continue;
        }
        entries.push(middleware_entry(argument, base + offset, scope));
    }
    entries
}

/// The `{…}` blocks of a file, as `(open, close)` byte pairs.
///
/// Router-scoped middleware is lexical here: an `r.Use(auth)` applies to the
/// routes registered on `r` after it *in the same block*. That is what makes
/// chi's idiomatic `r.Group(func(r chi.Router) { r.Use(auth); r.Get(...) })`
/// come out right — the closure's parameter shadows the outer `r`, and an
/// `auth` that leaked onto the routes after the group would report public
/// routes as authenticated, which is the one wrong answer about middleware
/// that gets acted on.
///
/// String literals are skipped with the same tracker every other scan here
/// uses. A brace in a comment is not, and an unbalanced one leaves its block
/// open to end of file — which widens a scope rather than dropping one.
fn brace_blocks(source: &str) -> Vec<(usize, usize)> {
    let mut stack = Vec::new();
    let mut blocks = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (offset, character) in source.char_indices() {
        if in_string_literal(character, &mut quote, &mut escaped) {
            continue;
        }
        match character {
            '{' => stack.push(offset),
            '}' => {
                if let Some(open) = stack.pop() {
                    blocks.push((open, offset));
                }
            }
            _ => {}
        }
    }
    blocks.extend(stack.into_iter().map(|open| (open, source.len())));
    blocks
}

/// Where the innermost block enclosing `at` closes; end of file at top level.
fn block_end(blocks: &[(usize, usize)], at: usize, source_len: usize) -> usize {
    blocks
        .iter()
        .filter(|(open, close)| *open < at && at < *close)
        .max_by_key(|(open, _)| *open)
        .map_or(source_len, |(_, close)| *close)
}

/// One router-level registration: `app.use(...)`, `r.Use(...)`.
struct RouterUse {
    receiver: String,
    /// Where the registration starts. It applies to routes after this.
    at: usize,
    /// Where its enclosing block closes. It applies to routes before this.
    scope_end: usize,
    /// Express's mount path: `app.use('/api', auth)` runs only under `/api`.
    prefix: Option<String>,
    middleware: Vec<ExtractedMiddleware>,
}

impl RouterUse {
    fn covers(&self, receiver: &str, at: usize, path: &str) -> bool {
        self.receiver == receiver
            && self.at < at
            && at < self.scope_end
            && self.prefix.as_deref().is_none_or(|prefix| {
                let prefix = prefix.trim_end_matches('/');
                prefix.is_empty()
                    || prefix == "*"
                    || path == prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
    }
}

/// A receiver bound from another: gin's `v1 := r.Group("/v1", auth)`, chi's
/// `admin := r.With(auth)`. The child runs the parent's middleware registered
/// before the derivation, then its own inline arguments.
struct Derivation {
    child: String,
    parent: String,
    at: usize,
    scope_end: usize,
    middleware: Vec<ExtractedMiddleware>,
}

/// Router-scoped middleware for a route on `receiver` at `at`, in run order.
///
/// Follows derivations back to their parents, bounded so a self-derivation or
/// a cycle (`r = r.With(x)` in a loop) terminates.
fn router_middleware(
    uses: &[RouterUse],
    derivations: &[Derivation],
    receiver: &str,
    at: usize,
    path: &str,
) -> Vec<ExtractedMiddleware> {
    const MAX_DERIVATION_HOPS: usize = 8;
    let mut chain = Vec::new();
    let (mut current, mut position) = (receiver.to_string(), at);
    for _ in 0..MAX_DERIVATION_HOPS {
        let own: Vec<ExtractedMiddleware> = uses
            .iter()
            .filter(|registration| registration.covers(&current, position, path))
            .flat_map(|registration| registration.middleware.iter().cloned())
            .collect();
        chain.push(own);
        // The nearest derivation of this receiver before the route, in scope.
        let Some(derivation) = derivations
            .iter()
            .filter(|d| d.child == current && d.at < position && position < d.scope_end)
            .max_by_key(|d| d.at)
        else {
            break;
        };
        chain.push(derivation.middleware.clone());
        current = derivation.parent.clone();
        position = derivation.at;
    }
    // Collected innermost-first; the parent's run first.
    chain.into_iter().rev().flatten().collect()
}

/// `app.use(...)` / `router.use(...)` on any receiver.
fn express_use_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)(?-u:\b)([A-Za-z_$][A-Za-z0-9_$]*(?:\.[A-Za-z_$][A-Za-z0-9_$]*)*)\.use\s*\("#,
        )
        .map_err(|error| format!("invalid Express middleware matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// The text of a string literal argument, if the argument is one.
fn string_literal(argument: &str) -> Option<&str> {
    let mut characters = argument.chars();
    let open = characters.next()?;
    if !matches!(open, '"' | '\'' | '`') || argument.len() < 2 || !argument.ends_with(open) {
        return None;
    }
    Some(&argument[1..argument.len() - 1])
}

/// Every `X.use(...)` registration in an Express file.
///
/// A receiver the file bound to a non-Express package is skipped for the same
/// reason [`non_router_receivers`] skips it for routes — `md.use(plugin)` is
/// markdown-it. A first argument that is a string is Express's mount path, and
/// one not starting with `/` means the call is not Express's `use` at all.
/// A mounted router (`app.use('/api', apiRouter)`) is recorded like any other
/// entry, because to Express it is one: a function in the stack, run for the
/// paths under its prefix.
fn express_router_uses(
    source: &str,
    not_routers: &std::collections::HashSet<String>,
    blocks: &[(usize, usize)],
) -> Result<Vec<RouterUse>, String> {
    let mut uses = Vec::new();
    for cap in express_use_re()?.captures_iter(source) {
        let (Some(full), Some(receiver)) = (cap.get(0), cap.get(1)) else {
            continue;
        };
        let root = receiver.as_str().split('.').next().unwrap_or_default();
        if not_routers.contains(root) {
            continue;
        }
        let Some((arguments, _)) = call_arguments(source, full.end()) else {
            continue;
        };
        let mut arguments = top_level_arguments(arguments);
        let prefix = match arguments
            .first()
            .and_then(|(_, first)| string_literal(first))
        {
            Some(path) if path.starts_with('/') || path == "*" => {
                let path = path.to_string();
                arguments.remove(0);
                Some(path)
            }
            Some(_) => continue,
            None => None,
        };
        let middleware = middleware_entries(&arguments, full.end(), MiddlewareScope::Router);
        if middleware.is_empty() {
            continue;
        }
        uses.push(RouterUse {
            receiver: receiver.as_str().to_string(),
            at: full.start(),
            scope_end: block_end(blocks, full.start(), source.len()),
            prefix,
            middleware,
        });
    }
    Ok(uses)
}

/// A Go route registration: any receiver, an optional chi `.With(...)` chain,
/// a verb or `Handle`/`HandleFunc`, and a string-literal pattern.
///
/// The verb set is chi's (`Get`, `Post`, …), gin's and echo's (`GET`, `POST`,
/// …, `Any`), and the `Handle`/`HandleFunc` pair net/http, gorilla/mux and chi
/// share. The receiver is not matched by name, for the reason the Express and
/// Python matchers are not: a router is whatever the author called it.
///
/// The word boundary is ASCII (`(?-u:\b)`), as in every matcher added beside
/// it. The identifiers it bounds are ASCII by construction, and a Unicode `\b`
/// makes the regex crate's lazy DFA give up at the first non-ASCII byte — a
/// comment in any other script — and finish the file on a slower engine. These
/// run on every Go, JavaScript and TypeScript file an index reads.
fn go_route_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            r#"(?m)(?-u:\b)([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)"#,
            r#"((?:\.With\((?:[^()]|\([^()]*\))*\))*)"#,
            r#"\.(Get|Post|Put|Delete|Patch|Head|Options|GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS|Any|HandleFunc|Handle)"#,
            r#"\s*\(\s*(?:"([^"\n]*)"|`([^`]*)`)\s*,"#,
        ))
        .map_err(|error| format!("invalid Go route matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// `r.Use(...)` on any receiver.
fn go_use_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?m)(?-u:\b)([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\.Use\s*\("#)
            .map_err(|error| format!("invalid Go middleware matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// `v1 := r.Group(...)` / `admin := r.With(...)` — a receiver derived from
/// another, which inherits its middleware.
fn go_derivation_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)(?-u:\b)([A-Za-z_][A-Za-z0-9_]*)\s*:?=\s*([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)\.(Group|With)\s*\("#,
        )
        .map_err(|error| format!("invalid Go router derivation matcher: {error}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Which Go router a file uses, read from its import paths.
///
/// Used for two things only: the `framework` label, and the one place the
/// frameworks disagree about argument order — echo's
/// `e.GET(path, handler, middleware...)` puts the handler *first*, where gin
/// puts it last and chi takes one. Never as a gate: a file that imports none of
/// these still has its registrations read, with gin's order and the label
/// `go`.
fn go_router_framework(source: &str) -> &'static str {
    const KNOWN: &[(&str, &str)] = &[
        ("\"github.com/labstack/echo", "echo"),
        ("\"github.com/gin-gonic/gin\"", "gin"),
        ("\"github.com/go-chi/chi", "chi"),
        ("\"github.com/gorilla/mux\"", "gorilla/mux"),
        ("\"net/http\"", "net/http"),
    ];
    KNOWN
        .iter()
        .find(|(import, _)| source.contains(import))
        .map_or("go", |(_, framework)| framework)
}

/// Whether a Go argument could be a handler: not a literal, not `nil`.
///
/// `cache.Get("/k", &out)` has a receiver, a verb, a `/` path and a second
/// argument, so the argument is what tells it from a route. An address-of is
/// accepted only by `Handle`, whose argument is an `http.Handler` value and is
/// routinely `&server{}`; on a verb method it is an out-parameter.
fn go_argument_is_a_handler(argument: &str, takes_handler_value: bool) -> bool {
    let Some(first) = argument.chars().next() else {
        return false;
    };
    if matches!(first, '"' | '`' | '\'') || first.is_ascii_digit() || first == '-' {
        return false;
    }
    if first == '&' && !takes_handler_value {
        return false;
    }
    !matches!(argument, "nil" | "true" | "false")
}

/// A Go handler argument's symbol: the final segment of an identifier path,
/// or nothing for a `func` literal or a wrapping call.
fn go_handler_name(argument: &str) -> String {
    if argument.contains('(') || argument.starts_with("func") {
        return String::new();
    }
    let (name, _) = middleware_reference(argument);
    name
}

/// The methods gorilla/mux chains after a registration:
/// `r.HandleFunc("/x", h).Methods("GET", "POST")`.
fn go_chained_methods(source: &str, call_end: usize) -> Vec<String> {
    let Some(rest) = source.get(call_end..) else {
        return Vec::new();
    };
    let Some(after) = rest.trim_start().strip_prefix(".Methods(") else {
        return Vec::new();
    };
    let start = source.len() - after.len();
    let Some((arguments, _)) = call_arguments(source, start) else {
        return Vec::new();
    };
    top_level_arguments(arguments)
        .into_iter()
        .filter_map(|(_, argument)| string_literal(argument))
        .filter(|method| !method.is_empty() && method.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_ascii_uppercase)
        .collect()
}

/// Go HTTP routes with their middleware.
///
/// Covers chi, gin, echo, gorilla/mux and net/http (including Go 1.22's
/// `"GET /path"` patterns), plus `r.Use(...)` and the `Group`/`With`
/// derivations that carry middleware to a child router. Not covered, and not
/// guessed at: chi's `r.Route("/api", func(r chi.Router) {...})` and gin's
/// `Group("/v1")` *path* prefixes (the route is recorded under its own path, as
/// an Express route on a mounted router already is), `Method("GET", ...)`,
/// gin's `Handle("GET", ...)`, and middleware applied by wrapping a handler in
/// a call (`mux.Handle("/x", auth(h))`), which has no registration to read.
fn go_routes(source: &str) -> Result<Vec<ExtractedRoute>, String> {
    let framework = go_router_framework(source);
    // Scopes are only consulted for a `Use` or a derivation, so a file with
    // neither — most Go files, including most route files — never pays for
    // the brace walk.
    let blocks = if go_use_re()?.is_match(source) || go_derivation_re()?.is_match(source) {
        brace_blocks(source)
    } else {
        Vec::new()
    };

    let mut uses = Vec::new();
    for cap in go_use_re()?.captures_iter(source) {
        let (Some(full), Some(receiver)) = (cap.get(0), cap.get(1)) else {
            continue;
        };
        let Some((arguments, _)) = call_arguments(source, full.end()) else {
            continue;
        };
        let middleware = middleware_entries(
            &top_level_arguments(arguments),
            full.end(),
            MiddlewareScope::Router,
        );
        if middleware.is_empty() {
            continue;
        }
        uses.push(RouterUse {
            receiver: receiver.as_str().to_string(),
            at: full.start(),
            scope_end: block_end(&blocks, full.start(), source.len()),
            prefix: None,
            middleware,
        });
    }

    let mut derivations = Vec::new();
    for cap in go_derivation_re()?.captures_iter(source) {
        let (Some(full), Some(child), Some(parent), Some(kind)) =
            (cap.get(0), cap.get(1), cap.get(2), cap.get(3))
        else {
            continue;
        };
        let Some((arguments, _)) = call_arguments(source, full.end()) else {
            continue;
        };
        let mut arguments = top_level_arguments(arguments);
        // gin's `Group(path, handlers...)`: the first argument is the prefix.
        if kind.as_str() == "Group" {
            match arguments.first().map(|(_, first)| *first) {
                Some(first) if string_literal(first).is_some() => {
                    arguments.remove(0);
                }
                // chi's `Group(func(r chi.Router) {...})` derives no receiver
                // by assignment; its closure is scoped by `brace_blocks`.
                _ => continue,
            }
        }
        derivations.push(Derivation {
            child: child.as_str().to_string(),
            parent: parent.as_str().to_string(),
            at: full.start(),
            scope_end: block_end(&blocks, full.start(), source.len()),
            middleware: middleware_entries(&arguments, full.end(), MiddlewareScope::Router),
        });
    }

    let mut routes = Vec::new();
    for cap in go_route_re()?.captures_iter(source) {
        let (Some(full), Some(receiver), Some(method)) = (cap.get(0), cap.get(1), cap.get(3))
        else {
            continue;
        };
        let Some(pattern) = cap.get(4).or_else(|| cap.get(5)).map(|m| m.as_str()) else {
            continue;
        };
        let method = method.as_str();
        let takes_pattern = matches!(method, "Handle" | "HandleFunc");
        // Go 1.22 `net/http` patterns may carry their method: `"GET /users"`.
        let (pattern_method, path) = match pattern.split_once(' ') {
            Some((verb, path))
                if takes_pattern
                    && !verb.is_empty()
                    && verb.chars().all(|c| c.is_ascii_uppercase()) =>
            {
                (Some(verb.to_string()), path.trim_start())
            }
            _ => (None, pattern),
        };
        if !path.starts_with('/') {
            continue;
        }
        let Some((arguments, call_end)) = call_arguments(source, full.end()) else {
            continue;
        };
        let arguments = top_level_arguments(arguments);
        let echo_order = framework == "echo";
        let handler_index = if echo_order { 0 } else { arguments.len() - 1 };
        let (_, handler) = arguments[handler_index];
        if !go_argument_is_a_handler(handler, method == "Handle") {
            continue;
        }
        let inline = if echo_order {
            &arguments[1..]
        } else {
            &arguments[..handler_index]
        };

        let methods = match method {
            "Handle" | "HandleFunc" => match pattern_method {
                Some(verb) => vec![verb],
                None => {
                    let chained = go_chained_methods(source, call_end);
                    if chained.is_empty() {
                        vec![UNSPECIFIED_METHOD.to_string()]
                    } else {
                        chained
                    }
                }
            },
            "Any" => vec![UNSPECIFIED_METHOD.to_string()],
            verb => vec![verb.to_ascii_uppercase()],
        };

        // Router-scoped first, then `.With(...)`, then the call's own: the
        // order the frameworks run them in.
        let mut middleware =
            router_middleware(&uses, &derivations, receiver.as_str(), full.start(), path);
        if let Some(chain) = cap.get(2).filter(|m| !m.as_str().is_empty()) {
            let mut cursor = chain.start();
            while let Some(found) = source[cursor..chain.end()].find(".With(") {
                let open = cursor + found + ".With(".len();
                let Some((arguments, end)) = call_arguments(source, open) else {
                    break;
                };
                middleware.extend(middleware_entries(
                    &top_level_arguments(arguments),
                    open,
                    MiddlewareScope::Route,
                ));
                cursor = end;
            }
        }
        middleware.extend(middleware_entries(
            inline,
            full.end(),
            MiddlewareScope::Route,
        ));

        for verb in methods {
            routes.push(ExtractedRoute {
                framework: framework.to_string(),
                http_method: verb,
                path_pattern: path.to_string(),
                handler_name: go_handler_name(handler),
                span: Span {
                    start_byte: full.start(),
                    end_byte: full.end(),
                },
                middleware: Some(middleware.clone()),
            });
        }
    }
    Ok(routes)
}

pub fn extract_framework_routes(
    framework_name: &str,
    source: &str,
) -> Result<Vec<ExtractedRoute>, String> {
    let mut routes = Vec::new();

    if framework_name == "python" || framework_name == "fastapi" || framework_name == "flask" {
        for cap in python_route_re()?.captures_iter(source) {
            let Some(full) = cap.get(0) else {
                continue;
            };
            // The decorator's own call: its arguments carry `methods=`, and the
            // function it decorates starts after the call closes rather than
            // after its path literal — a decorator spread over several lines
            // otherwise finds `methods=[...]` where it expects the `def`, and
            // binds to nothing.
            let call = call_arguments(source, full.end());
            let handler = python_decorated_handler(source, call.map_or(full.end(), |(_, end)| end))
                .unwrap_or_default();
            for method in python_route_methods(&cap[1], call.map(|(arguments, _)| arguments)) {
                routes.push(ExtractedRoute {
                    framework: "fastapi/flask".to_string(),
                    http_method: method,
                    path_pattern: cap[2].to_string(),
                    handler_name: handler.clone(),
                    span: Span {
                        start_byte: full.start(),
                        end_byte: full.end(),
                    },
                    // No producer: Flask's `before_request` and FastAPI's
                    // `Depends` are not read, so middleware is unknown here,
                    // not absent.
                    middleware: None,
                });
            }
        }
    }

    if framework_name == "python" || framework_name == "django" {
        // Django's routes are calls in a `urlpatterns` list rather than
        // decorators, so unlike every matcher above this one asks the file to
        // identify itself before reading an ordinary `path(...)` call as a
        // route.
        //
        // Only `path()` and `re_path()` are matched. Django's legacy `url()`
        // alias is not: it was removed in Django 4, and `url(` is common enough
        // elsewhere that matching it would bind routes to symbols serving no
        // HTTP path.
        if is_django_urlconf(source) {
            for cap in django_re()?.captures_iter(source) {
                let (Some(full), Some(name), Some(pattern)) = (cap.get(0), cap.get(1), cap.get(2))
                else {
                    continue;
                };
                // The view is the first positional argument after the pattern.
                // An entry with none registers no view — a `path()` call this
                // pass misread, or one passing only keywords.
                let Some(target) = call_arguments(source, full.end())
                    .and_then(|(arguments, _)| first_positional_argument(arguments))
                else {
                    continue;
                };
                if mounts_included_urlconf(target) {
                    continue;
                }
                routes.push(ExtractedRoute {
                    framework: "django".to_string(),
                    // Neither `path()` nor `re_path()` declares a verb: Django
                    // dispatches on it inside the view, or through a
                    // class-based view's own `get`/`post` methods. Naming one
                    // would be a claim the URLconf does not make.
                    http_method: UNSPECIFIED_METHOD.to_string(),
                    path_pattern: if name.as_str() == "re_path" {
                        django_regex_to_template(pattern.as_str())
                    } else {
                        pattern.as_str().to_string()
                    },
                    handler_name: view_symbol_name(target).unwrap_or_default(),
                    span: Span {
                        // The match consumes the character before the call, so
                        // the span opens on the callee rather than on whatever
                        // separator preceded it.
                        start_byte: name.start(),
                        end_byte: full.end(),
                    },
                    // Django's middleware is a settings list, not a URLconf
                    // fact; no producer reads it.
                    middleware: None,
                });
            }
        }
    }

    if framework_name == "rust" || framework_name == "axum" {
        for cap in axum_re()?.captures_iter(source) {
            let Some(full) = cap.get(0) else {
                continue;
            };
            routes.push(ExtractedRoute {
                framework: "axum".to_string(),
                http_method: cap[2].to_uppercase(),
                path_pattern: cap[1].to_string(),
                handler_name: cap[3].to_string(),
                span: Span {
                    start_byte: full.start(),
                    end_byte: full.end(),
                },
                // Axum layers (`.layer(...)`) are not read.
                middleware: None,
            });
        }
    }

    if framework_name == "go" {
        routes.extend(go_routes(source)?);
    }

    if framework_name == "javascript"
        || framework_name == "typescript"
        || framework_name == "express"
    {
        // Scanned once per file, not once per site: the bindings are a property
        // of the file, and a site-by-site scan would be quadratic in a server
        // that registers many routes.
        let not_routers = non_router_receivers(source);
        // Only paid for when the file registers middleware at all.
        let blocks = if express_use_re()?.is_match(source) {
            brace_blocks(source)
        } else {
            Vec::new()
        };
        let uses = express_router_uses(source, &not_routers, &blocks)?;
        for cap in express_re()?.captures_iter(source) {
            let (Some(full), Some(receiver)) = (cap.get(0), cap.get(1)) else {
                continue;
            };
            // `axios.get('/api/users', config)` is shaped exactly like a route
            // and is not one. The file's own bindings say so: only the root of
            // the receiver carries the binding, since `client.api.get(...)`
            // reaches through whatever `client` was bound to.
            let root = receiver.as_str().split('.').next().unwrap_or_default();
            if not_routers.contains(root) {
                continue;
            }
            // Express takes callbacks after the path and nothing else, so an
            // options object in the final position means this call belongs to
            // some other API.
            //
            // A call whose arguments cannot be read — unbalanced, or longer
            // than the shared scan cap — is left as it was found rather than
            // dropped: the check did not run, and reporting "not a handler" for
            // a list nobody could read would turn an unread call into a
            // deletion. The path and the trailing comma already matched.
            let readable_arguments = call_arguments(source, full.end())
                .map(|(arguments, _)| top_level_arguments(arguments));
            if matches!(
                readable_arguments.as_deref().and_then(<[_]>::last),
                Some((_, last)) if !express_final_argument_is_a_handler(last)
            ) {
                continue;
            }
            // Everything between the path and the handler is middleware, and
            // so is what the router registered before this route. An argument
            // list nobody could read leaves the route's own entries unknown,
            // so the whole field is: a list missing its inline half would
            // read as complete.
            let path = &cap[3];
            let middleware = readable_arguments.map(|arguments| {
                let mut middleware =
                    router_middleware(&uses, &[], receiver.as_str(), full.start(), path);
                middleware.extend(middleware_entries(
                    &arguments[..arguments.len() - 1],
                    full.end(),
                    MiddlewareScope::Route,
                ));
                middleware
            });
            routes.push(ExtractedRoute {
                framework: "express".to_string(),
                http_method: cap[2].to_uppercase(),
                path_pattern: path.to_string(),
                handler_name: express_handler_name(source, full.end()).unwrap_or_default(),
                span: Span {
                    start_byte: full.start(),
                    end_byte: full.end(),
                },
                middleware,
            });
        }
    }

    Ok(routes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fastapi_route() {
        let src = r#"@app.get("/health")
def health():
    return "ok"
"#;
        let routes = extract_framework_routes("python", src).unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].path_pattern, "/health");
        assert_eq!(routes[0].http_method, "GET");
    }
}

#[cfg(test)]
mod language_dispatch_tests {
    use super::*;

    /// Each framework matcher runs for its own languages and no others.
    ///
    /// Every `==` and `||` in the language dispatch was mutable without a
    /// failure. Collapsing a disjunction to `&&` makes a matcher unreachable —
    /// route handlers stop being extracted, and since a route edge is what
    /// keeps a handler live, every handler in that language becomes false-dead.
    /// Inverting an equality runs a matcher against the wrong grammar's source.
    #[test]
    fn route_matchers_dispatch_on_their_own_languages() {
        let python = "@app.get('/items')\ndef read_items():\n    return []\n";
        let rust = "async fn handler() {}\nfn app() -> Router { Router::new().route(\"/x\", get(handler)) }\n";
        let express = "const app = express();\napp.get('/users', function handler(req, res) {});\n";

        // Each alias for a language reaches the same matcher.
        for name in ["python", "fastapi", "flask"] {
            let routes = extract_framework_routes(name, python).expect("python matcher runs");
            assert!(
                !routes.is_empty(),
                "{name} must extract a Python route: {routes:?}"
            );
        }
        for name in ["rust", "axum"] {
            let routes = extract_framework_routes(name, rust).expect("rust matcher runs");
            assert!(!routes.is_empty(), "{name} must extract an axum route");
        }
        for name in ["javascript", "typescript", "express"] {
            let routes = extract_framework_routes(name, express).expect("js matcher runs");
            assert!(!routes.is_empty(), "{name} must extract an express route");
        }

        // A matcher must not claim source from another language: Python
        // decorator syntax is not a Rust or JS route.
        let cross = extract_framework_routes("rust", python).expect("no error");
        assert!(
            cross.is_empty(),
            "the Rust matcher must not extract Python decorators: {cross:?}"
        );

        // An unknown language yields nothing rather than guessing.
        let unknown = extract_framework_routes("cobol", python).expect("no error");
        assert!(
            unknown.is_empty(),
            "an unhandled language extracts no routes"
        );
    }
}

#[cfg(test)]
mod handler_name_tests {
    use super::*;

    /// A Python route names the function its decorator is attached to.
    ///
    /// This previously recorded the literal string `decorated_handler`, which
    /// matches no symbol — so no FastAPI or Flask route ever linked to its
    /// handler and every HTTP-only endpoint looked uncalled. The scan skips
    /// blank lines, comments and stacked decorators, because a route decorator
    /// is rarely the last one before the `def`.
    #[test]
    fn a_python_route_names_the_function_it_decorates() {
        let handler = |source: &str| {
            extract_framework_routes("python", source)
                .expect("matcher compiles")
                .into_iter()
                .map(|route| route.handler_name)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            handler("@app.get('/items')\ndef read_items():\n    return []\n"),
            ["read_items"]
        );
        assert_eq!(
            handler("@router.post('/x')\nasync def create_x():\n    return 1\n"),
            ["create_x"],
            "an async handler is still a handler"
        );
        assert_eq!(
            handler("@app.get('/y')\n@requires_auth\n# comment\ndef guarded():\n    return 1\n"),
            ["guarded"],
            "stacked decorators and comments sit between the route and its function"
        );

        // A decorator not attached to a function names nothing rather than
        // guessing — a route bound to the wrong symbol is worse than an unbound
        // one, because it also protects that symbol from dead-code reporting.
        assert_eq!(
            handler("@app.get('/z')\nCONST = 1\n"),
            [""],
            "an unattached decorator must not invent a handler"
        );
    }

    /// Nesting inside an Express argument list never ends the scan early.
    ///
    /// The handler is the last *top-level* argument, so both the search for the
    /// call's closing paren and the split into arguments track nesting depth.
    /// Every depth adjustment was mutable: without them a comma inside an arrow
    /// function body, a nested call, an array of middleware or an options
    /// object ends the scan at the wrong place, and the route binds to a
    /// fragment of an inner expression instead of to its handler.
    #[test]
    fn nesting_inside_an_express_argument_list_does_not_end_the_scan() {
        let handler = |source: &str| {
            extract_framework_routes("javascript", source)
                .expect("matcher compiles")
                .into_iter()
                .map(|route| route.handler_name)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            handler("app.get('/x', wrap(inner), handleUsers);\n"),
            ["handleUsers"],
            "a nested call's own parentheses must not close the argument list"
        );
        assert_eq!(
            handler("app.get('/x', (req, res) => { helper(); }, handleUsers);\n"),
            ["handleUsers"],
            "an arrow body's commas and braces are not argument separators"
        );
        assert_eq!(
            handler("app.get('/x', [auth, log], handleUsers);\n"),
            ["handleUsers"],
            "an array of middleware is one argument"
        );
        assert_eq!(
            handler("app.get('/x', { a: 1 }, handleUsers);\n"),
            ["handleUsers"],
            "an options object is one argument"
        );

        // When the *last* argument is the anonymous one, there is still no name
        // — the scan must reach it correctly and then report nothing.
        assert_eq!(
            handler("app.get('/x', auth, (req, res) => { go(); });\n"),
            [""],
            "middleware before an anonymous handler still yields no name"
        );
        assert_eq!(
            handler("app.get('/x', (req, res) => { helper(); });\n"),
            [""]
        );
    }

    /// An Express route names its handler argument, after any middleware.
    ///
    /// This previously recorded `anonymous_or_function` for every route. The
    /// handler is the last argument; earlier ones are middleware. A genuinely
    /// anonymous handler yields an empty name, which resolves to nothing —
    /// deliberately, since a placeholder would bind to any symbol sharing it.
    #[test]
    fn an_express_route_names_its_handler_argument() {
        let handler = |source: &str| {
            extract_framework_routes("javascript", source)
                .expect("matcher compiles")
                .into_iter()
                .map(|route| route.handler_name)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            handler("app.get('/users', handleUsers);\n"),
            ["handleUsers"]
        );
        assert_eq!(
            handler("app.get('/u', auth, requireAdmin, handleUsers);\n"),
            ["handleUsers"],
            "the handler is the last argument, not the first"
        );
        assert_eq!(
            handler("app.get('/f', function namedHandler(req, res) {});\n"),
            ["namedHandler"],
            "a named function expression names the handler"
        );
        assert_eq!(
            handler("app.get('/m', ctrl.handleThing);\n"),
            ["handleThing"],
            "a member expression contributes its final segment, which is what the \
             symbol index knows"
        );

        // Anonymous handlers have no name, and must not be given one.
        assert_eq!(
            handler("app.get('/a', (req, res) => { res.send(1); });\n"),
            [""],
            "an arrow function handler is anonymous"
        );
        assert_eq!(handler("app.get('/g', function (req, res) {});\n"), [""]);
    }
}

#[cfg(test)]
mod flask_route_tests {
    use super::*;

    /// Flask's classic decorator is `@app.route`, whose verb is not a verb.
    ///
    /// The Python matcher listed only the seven HTTP-verb decorators, so
    /// `@app.route("/x")` — the form every classic Flask app is written in —
    /// matched nothing. No `ExtractedRoute` meant no `HandlesRoute` edge, and
    /// that edge is what tells liveness a handler is reached from outside the
    /// call graph.
    ///
    /// The verb comes from `methods=`. Without it the rule accepts GET *and*
    /// HEAD, so the route records `ANY`: naming one verb would be a claim the
    /// source does not make.
    #[test]
    fn extracts_flask_route() {
        let src = r#"@app.route("/api/users/<uid>")
def get_user(uid):
    return uid
"#;
        let routes = extract_framework_routes("python", src).unwrap();
        assert_eq!(routes.len(), 1, "{routes:?}");
        assert_eq!(routes[0].path_pattern, "/api/users/<uid>");
        assert_eq!(routes[0].http_method, "ANY");
        assert_eq!(routes[0].handler_name, "get_user");
    }

    fn python_routes(source: &str) -> Vec<(String, String, String)> {
        extract_framework_routes("python", source)
            .expect("matcher compiles")
            .into_iter()
            .map(|route| (route.http_method, route.path_pattern, route.handler_name))
            .collect()
    }

    fn methods_of(source: &str) -> Vec<String> {
        python_routes(source)
            .into_iter()
            .map(|(method, _, _)| method)
            .collect()
    }

    /// A Flask route's verbs are exactly the ones `methods=` declares.
    ///
    /// One route per declared verb, because the verb is half a route's
    /// identity downstream: the resolver keys the edge on `"{method} {path}"`,
    /// and the API-route consumer matches a client call's verb against it. A
    /// single route carrying `"GET,POST"` would match neither.
    #[test]
    fn a_flask_routes_verbs_come_from_its_methods_argument() {
        assert_eq!(
            python_routes("@app.route('/x', methods=['POST'])\ndef create():\n    return 1\n"),
            [("POST".into(), "/x".into(), "create".into())]
        );
        assert_eq!(
            methods_of("@app.route('/x', methods=['GET', 'POST'])\ndef both():\n    return 1\n"),
            ["GET", "POST"],
            "both declared verbs are recorded, one route each"
        );
        assert_eq!(
            methods_of("@app.route('/x', methods=(\"put\",))\ndef replace():\n    return 1\n"),
            ["PUT"],
            "a tuple is the same declaration, and the verb is normalized"
        );
        assert_eq!(
            methods_of("@app.route('/x', methods=['GET', 'GET'])\ndef once():\n    return 1\n"),
            ["GET"],
            "a repeated verb is one route, not two identical ones"
        );

        // What the source does not say, the route does not claim.
        assert_eq!(
            methods_of("@app.route('/x', methods=ALLOWED)\ndef guarded():\n    return 1\n"),
            ["ANY"],
            "a non-literal methods= names no verb this pass can read"
        );
        assert_eq!(
            methods_of("@app.route('/x', methods=[])\ndef empty():\n    return 1\n"),
            ["ANY"],
            "an empty list declares no verb"
        );
        assert_eq!(
            methods_of(
                "@app.route('/x', methods=['GET'] if debug else ['POST'])\ndef either():\n    return 1\n"
            ),
            ["ANY"],
            "a list that is one operand of a larger expression is not the verb set"
        );
        assert_eq!(
            methods_of("@app.route('/x', methods=['GET'\ndef truncated():\n    return 1\n"),
            ["ANY"],
            "a list the source never closes is read as no verb, not as its first"
        );

        // A literal followed by another keyword argument is still just the
        // literal.
        assert_eq!(
            methods_of(
                "@app.route('/x', methods=['POST'], strict_slashes=False)\ndef create():\n    return 1\n"
            ),
            ["POST"]
        );
    }

    /// `methods=` counts only as the decorator call's own keyword argument.
    ///
    /// Everything here parses as a Flask route with no verb declared. Reading
    /// any of them as a verb would put a route on the graph under a method the
    /// app never registered, which is worse than the `ANY` the source supports.
    #[test]
    fn a_methods_keyword_must_belong_to_the_decorator_itself() {
        assert_eq!(
            methods_of("@app.route('/x', endpoint=\"methods=['POST']\")\ndef f():\n    return 1\n"),
            ["ANY"],
            "text inside a string literal is not an argument"
        );
        assert_eq!(
            methods_of(
                "@app.route('/x', defaults={'methods': ['POST']})\ndef f():\n    return 1\n"
            ),
            ["ANY"],
            "a key nested inside another argument is not the call's keyword"
        );
        assert_eq!(
            methods_of("@app.route('/x', allowed_methods=['POST'])\ndef f():\n    return 1\n"),
            ["ANY"],
            "a longer keyword that merely ends in `methods` is a different one"
        );
        assert_eq!(
            methods_of(
                "@app.route('/a')\ndef a():\n    return 1\n\n\
                 @app.route('/b', methods=['POST'])\ndef b():\n    return 2\n"
            ),
            ["ANY", "POST"],
            "a later decorator's methods= must not reach an earlier route"
        );
        assert_eq!(
            methods_of("@app.route('/x', defaults={'p': ')'}, methods=['DELETE'])\ndef f():\n    return 1\n"),
            ["DELETE"],
            "a paren inside a string literal does not end the argument list"
        );
    }

    /// A decorator spread over several lines still names the function below it.
    ///
    /// The handler scan starts after the decorator call's closing paren, not
    /// after its path literal — otherwise the scan's first line is
    /// `methods=[...]`, which is not a `def`, and the route binds to nothing.
    /// The multi-line form is ordinary Flask, and a route that names no handler
    /// produces no edge at all.
    #[test]
    fn a_multi_line_decorator_still_names_its_handler() {
        assert_eq!(
            python_routes(
                "@app.route(\n    '/x',\n    methods=['PATCH'],\n)\ndef patch_it():\n    return 1\n"
            ),
            [("PATCH".into(), "/x".into(), "patch_it".into())]
        );
        assert_eq!(
            python_routes("@app.get(\n    '/y',\n)\nasync def read_y():\n    return 1\n"),
            [("GET".into(), "/y".into(), "read_y".into())],
            "a verb decorator spread over lines has the same gap"
        );
    }

    /// An Express argument list is read past a bracket inside a string.
    ///
    /// The Express handler scan and the Flask keyword scan share one routine
    /// for finding where a call's arguments end. Making it skip string
    /// contents — which the Flask side needs, since `methods=` can follow an
    /// argument containing a paren — also closes the same hole on the Express
    /// side, where a `)` inside a string used to end the scan early and leave
    /// the route bound to a fragment instead of its handler.
    #[test]
    fn an_express_argument_list_is_read_past_a_bracket_inside_a_string() {
        let handlers = |source: &str| {
            extract_framework_routes("javascript", source)
                .expect("matcher compiles")
                .into_iter()
                .map(|route| route.handler_name)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            handlers("app.get('/x', mw(')'), handleUsers);\n"),
            ["handleUsers"],
            "a paren inside a string argument is not the end of the call"
        );
        assert_eq!(
            handlers("app.get('/x', \"it's\", handleUsers);\n"),
            ["handleUsers"],
            "an apostrophe inside a double-quoted string opens nothing"
        );
    }

    /// A call whose closing paren never arrives does not scan the whole file.
    ///
    /// The argument scan runs once per route match, so an unbalanced paren in a
    /// file of many decorators would make extraction quadratic. Past the cap the
    /// call reads as unfinished: no arguments to read a verb from, and — for
    /// Python — the handler scan falls back to the line after the decorator,
    /// which is where it looked before there was an argument scan at all.
    #[test]
    fn an_unclosed_call_stops_scanning_at_the_cap() {
        let filler = "x".repeat(MAX_CALL_SCAN + 1024);

        assert_eq!(
            python_routes(&format!(
                "@app.route('/x', {filler} methods=['POST'])\ndef f():\n    return 1\n"
            )),
            [("ANY".to_string(), "/x".to_string(), "f".to_string())],
            "an unreadable argument list declares no verb, and does not lose the route"
        );

        let express = extract_framework_routes(
            "javascript",
            &format!("app.get('/x', {filler}, handleUsers);\n"),
        )
        .expect("matcher compiles");
        assert_eq!(
            express
                .into_iter()
                .map(|route| route.handler_name)
                .collect::<Vec<_>>(),
            [""],
            "an argument list longer than the cap names no handler"
        );
    }

    /// A route decorator's receiver is whatever the author named the object.
    ///
    /// The matcher used to hard-code `app`, `router` or `api`, which is not how
    /// either framework is written: a Flask blueprint is `bp = Blueprint(...)`
    /// and a FastAPI router is `users = APIRouter()`. A module holding only
    /// blueprint routes produced no routes at all — and `devmap routes --json`
    /// reported `count: 0` under a scan claiming `complete: true`, which is an
    /// answer that ran and is wrong presented as one that is whole.
    ///
    /// Losing the route loses the handler's only inbound edge, so the endpoint
    /// is absent from `routes`, `api-impact`, `shape-check`, `cypher` and the
    /// `HandlesRoute` edges the graph carries.
    #[test]
    fn a_route_decorators_receiver_may_be_any_name() {
        assert_eq!(
            python_routes(
                "@bp.route(\"/users\")\ndef list_users():\n    return []\n\n\
                 @bp.post(\"/users/new\")\ndef create_user():\n    return {}\n"
            ),
            [
                ("ANY".into(), "/users".into(), "list_users".into()),
                ("POST".into(), "/users/new".into(), "create_user".into())
            ],
            "a blueprint-only module holds real routes, and must not answer none"
        );
        assert_eq!(
            python_routes("@users.get(\"/items\")\ndef list_items():\n    return []\n"),
            [("GET".into(), "/items".into(), "list_items".into())],
            "a FastAPI router bound to its own name is still a router"
        );
        assert_eq!(
            python_routes("@api_v2.router.get(\"/x\")\ndef read_x():\n    return 1\n"),
            [("GET".into(), "/x".into(), "read_x".into())],
            "a router reached through an attribute chain is still a router"
        );

        // The mixed module that reproduced the defect: one of its four routes
        // survived the receiver allow-list.
        assert_eq!(
            python_routes(
                "@app.route(\"/health\")\ndef health():\n    return \"ok\"\n\n\
                 @bp.route(\"/users\")\ndef list_users():\n    return []\n\n\
                 @users.get(\"/items\")\ndef list_items():\n    return []\n\n\
                 @bp.post(\"/users/new\")\ndef create_user():\n    return {}\n"
            )
            .len(),
            4,
            "every route in a mixed module is extracted, not only the `app` one"
        );
    }

    /// A Python decorator may target a class, and the handler is then the class.
    ///
    /// Python's grammar is `decorated: decorators (classdef | funcdef |
    /// async_funcdef)`, so `class` is one of exactly three things a decorator
    /// can be applied to. Reading only `def` and `async def` left the third
    /// resolving to `""`, which `resolver.rs`'s route arm treats as a
    /// deliberately anonymous Express arrow function and skips without a
    /// ledger entry — a handler that could not be extracted reporting the same
    /// outcome as one that has no name by design.
    ///
    /// Measured against seven `site-packages` trees: 103 route-decorator
    /// matches, none of them over a class, so this is a latent shape rather
    /// than an observed loss. It is fixed because the omission is a missing
    /// grammar case and not a judgement call, and because the silence is the
    /// expensive part.
    #[test]
    fn a_route_decorator_over_a_class_names_the_class_as_its_handler() {
        assert_eq!(
            python_routes(
                "@app.route(\"/users\")\nclass UserView(MethodView):\n    \
                 def get(self):\n        return []\n"
            ),
            [("ANY".into(), "/users".into(), "UserView".into())],
            "a class-based view is the route's handler, not an empty name"
        );
        // This assertion is also the composition guard for the two halves of
        // this pass, and the receiver `bp` is load-bearing rather than
        // decorative: it needs the widened receiver *and* the `class` prefix
        // together. Verified by mutation — restoring the old `(app|router|api)`
        // allow-list while keeping the `class` prefix fails here with `left:
        // []`, no route at all, and keeping the allow-list wide while dropping
        // `class` yields `("POST", "/items", "")`. Do not "simplify" the
        // receiver to `app`; that silently drops the intersection to whichever
        // half is still present.
        assert_eq!(
            python_routes(
                "@bp.post(\"/items\")\n@login_required\nclass Create(MethodView):\n    \
                 pass\n"
            ),
            [("POST".into(), "/items".into(), "Create".into())],
            "stacked decorators above a class are skipped the same as above a def"
        );
    }

    /// What tells a route decorator from a look-alike: the shape of its path.
    ///
    /// Once the receiver stops being an allow-list, the receiver's name carries
    /// no information, so precision has to come from somewhere the frameworks
    /// themselves define. Werkzeug's `Rule.__init__` raises `ValueError` unless
    /// the rule starts with `/`, and Starlette's `Route.__init__` asserts
    /// `path.startswith("/")` — so a decorator whose first argument is a string
    /// literal not starting with `/` is not a route in either framework.
    ///
    /// `@mock.patch` is the case that needs it: an identifier receiver, an
    /// HTTP-verb attribute, a string literal first argument and a decorated
    /// `def` — every other signal admits it. A false `HandlesRoute` edge is not
    /// merely a wrong row in a listing: it is what tells liveness a symbol is
    /// reached from outside the call graph, so it also silences a genuinely
    /// dead symbol.
    #[test]
    fn a_decorators_path_must_be_one_the_framework_would_accept() {
        for (source, why) in [
            (
                "@mock.patch(\"os.path.exists\")\ndef test_it(exists):\n    return 1\n",
                "a patch target is not a route path: no framework would accept it",
            ),
            (
                "@cache.get(\"session:1\")\ndef loader():\n    return 1\n",
                "a cache key is not a route path",
            ),
            (
                "# @app.route(\"/x\")\ndef handle():\n    return 1\n",
                "a commented-out route decorates nothing, and must not bind a \
                 handler nothing reaches any more",
            ),
            (
                "value = registry.get(\"/x\")\ndef handle():\n    return 1\n",
                "a decorator can begin nowhere but the start of a line",
            ),
        ] {
            assert!(
                python_routes(source).is_empty(),
                "{why}: {:?}",
                python_routes(source)
            );
        }

        // An indented decorator is still a decorator: a router registered on a
        // method inside a class is ordinary FastAPI.
        assert_eq!(
            python_routes(
                "class Api:\n    @router.get(\"/x\")\n    def read_x(self):\n        return 1\n"
            ),
            [("GET".into(), "/x".into(), "read_x".into())],
            "leading indentation does not stop a decorator from being one"
        );
    }
}

#[cfg(test)]
mod django_route_tests {
    use super::*;

    const URLCONF_HEAD: &str = "from django.urls import include, path, re_path\n\
                                from . import views\n\n";

    fn urlconf(entries: &str) -> String {
        format!("{URLCONF_HEAD}urlpatterns = [\n{entries}]\n")
    }

    fn django_routes(source: &str) -> Vec<(String, String, String)> {
        extract_framework_routes("python", source)
            .expect("matcher compiles")
            .into_iter()
            .map(|route| (route.http_method, route.path_pattern, route.handler_name))
            .collect()
    }

    /// Django's URLconf had no matcher at all, so its endpoints were invisible.
    ///
    /// `path()` and `re_path()` are calls in a `urlpatterns` list, not
    /// decorators, so nothing here matched them. A Django view still picked up
    /// a `References` edge from urls.py and so was never reported dead — but no
    /// `HandlesRoute` edge existed, which is what carries a route's path and
    /// verb, so the endpoint had no row in the route map and no presence in the
    /// API-impact surface.
    #[test]
    fn extracts_django_route() {
        let source =
            urlconf("    path(\"users/<int:pk>/\", views.user_detail, name=\"user-detail\"),\n");
        let routes = extract_framework_routes("python", &source).unwrap();
        assert_eq!(routes.len(), 1, "{routes:?}");
        assert_eq!(routes[0].framework, "django");
        assert_eq!(routes[0].path_pattern, "users/<int:pk>/");
        assert_eq!(routes[0].handler_name, "user_detail");

        // A URLconf entry names no verb — Django dispatches on it inside the
        // view — so the route records the wildcard the verb matcher already
        // understands rather than inventing GET.
        assert_eq!(routes[0].http_method, UNSPECIFIED_METHOD);
        assert_eq!(routes[0].http_method, "ANY");

        // The span opens on the callee: the pattern consumes the separator in
        // front of it, and a span starting one byte early would point at the
        // list's indentation instead of at the route.
        assert_eq!(
            &source[routes[0].span.start_byte..routes[0].span.start_byte + 4],
            "path"
        );
    }

    /// `re_path` declares a regex, and the graph downstream reads templates.
    ///
    /// `normalize_route_path` substitutes `<name>` and knows nothing of
    /// `(?P<name>...)`, so an unconverted regex reaches it as several path
    /// segments that match no client call — and anchors become segments of
    /// their own. Converting here keeps regex syntax in the one matcher that
    /// can produce it.
    #[test]
    fn a_re_path_regex_is_rewritten_as_a_path_template() {
        assert_eq!(
            django_routes(&urlconf(
                "    re_path(r\"^legacy/(?P<pk>\\d+)/$\", views.legacy_detail),\n"
            )),
            [(
                "ANY".to_string(),
                "legacy/<pk>/".to_string(),
                "legacy_detail".to_string()
            )],
            "anchors are dropped and a named group becomes its parameter"
        );

        let paths = |entry: &str| -> Vec<String> {
            django_routes(&urlconf(entry))
                .into_iter()
                .map(|(_, path, _)| path)
                .collect()
        };

        assert_eq!(
            paths("    re_path(r\"^legacy/(\\d+)/$\", views.by_position),\n"),
            ["legacy/<arg>/"],
            "an unnamed group is a parameter Django passes by position"
        );
        assert_eq!(
            paths("    re_path(r\"^files/(?P<name>[\\w-]+)\\.json$\", views.blob),\n"),
            ["files/<name>.json"],
            "a class inside a group does not end it, and an escaped dot is a dot"
        );
        assert_eq!(
            paths("    re_path(r\"^x/(?P<slug>[\\w-]+(?:-\\d+)?)/$\", views.slug),\n"),
            ["x/<slug>/"],
            "a nested group does not close the named group early"
        );

        // What has no template equivalent is carried through as written rather
        // than rewritten into a shape the app does not serve.
        assert_eq!(
            paths("    re_path(r\"^x/(?:edit|delete)/$\", views.act),\n"),
            ["x/(?:edit|delete)/"],
            "a non-capturing group captures nothing, so it is not a parameter"
        );
        assert_eq!(
            paths("    re_path(r\"^x/\\d+/$\", views.bare),\n"),
            ["x/\\d+/"],
            "a class outside any group has no name to become"
        );

        // A `path()` pattern is already a template and must not be put through
        // the regex rewriter: its characters are literal.
        assert_eq!(
            paths("    path(\"^caret/\", views.literal),\n"),
            ["^caret/"],
            "path() patterns are templates, so nothing in them is regex syntax"
        );
    }

    /// A class-based view is registered through Django's `as_view()` accessor.
    ///
    /// The symbol the index knows is the class. Taking the final dotted segment
    /// verbatim would bind every class-based route to `as_view` — one name, so
    /// every such route would resolve to the same wrong symbol or to none.
    #[test]
    fn a_class_based_view_binds_to_its_class() {
        let handlers = |entry: &str| -> Vec<String> {
            django_routes(&urlconf(entry))
                .into_iter()
                .map(|(_, _, handler)| handler)
                .collect()
        };

        assert_eq!(
            handlers("    path(\"users/\", views.UserList.as_view()),\n"),
            ["UserList"]
        );
        assert_eq!(
            handlers("    path(\"users/\", UserList.as_view(), name=\"users\"),\n"),
            ["UserList"],
            "an unqualified class is the same registration"
        );
        assert_eq!(
            handlers("    path(\"users/\", views.UserList.as_view(paginate_by=10)),\n"),
            ["UserList"],
            "arguments to as_view() do not change which symbol is registered"
        );
        assert_eq!(
            handlers("    path(\"x/\", views.detail),\n"),
            ["detail"],
            "a function view is still its final segment"
        );

        // A view this pass cannot name records no name, rather than a fragment
        // of one that would resolve to an unrelated symbol.
        assert_eq!(
            handlers("    path(\"x/\", login_required(views.detail)),\n"),
            [""],
            "a wrapped view names the wrapper's call, not a symbol"
        );
        assert_eq!(handlers("    path(\"x/\", lambda request: None),\n"), [""]);
    }

    /// `include()` mounts a whole URLconf, and is skipped rather than guessed.
    ///
    /// Following it means reading the included module and prefixing every path
    /// it declares. This pass does not do that, so recording the entry would
    /// put a prefix on the graph that no handler serves, and binding it would
    /// make `include` itself the handler of every mounted route.
    #[test]
    fn an_include_entry_is_skipped_rather_than_mis_prefixed() {
        assert_eq!(
            django_routes(&urlconf("    path(\"api/\", include(\"app.urls\")),\n")),
            [],
            "a mounted URLconf is not a route this pass can resolve"
        );
        assert_eq!(
            django_routes(&urlconf(
                "    path(\"api/\", django.urls.include(\"app.urls\")),\n"
            )),
            [],
            "a qualified include is the same call"
        );

        // Skipping the mount must not skip the real routes beside it.
        assert_eq!(
            django_routes(&urlconf(
                "    path(\"api/\", include(\"app.urls\")),\n    path(\"health/\", views.health),\n"
            )),
            [("ANY".into(), "health/".into(), "health".into())],
            "an entry after an include is still extracted"
        );

        // A view whose name merely ends in `include` is not the `include` call.
        assert_eq!(
            django_routes(&urlconf("    path(\"x/\", views.include_report),\n")),
            [("ANY".into(), "x/".into(), "include_report".into())]
        );
    }

    /// An entry with no positional view registers nothing to bind.
    #[test]
    fn an_entry_without_a_view_records_no_route() {
        assert_eq!(
            django_routes(&urlconf("    path(\"x/\"),\n")),
            [],
            "a pattern alone is not a route"
        );
        assert_eq!(
            django_routes(&urlconf("    path(\"x/\", name=\"x\"),\n")),
            [],
            "a keyword argument is not the view"
        );

        // The root of an app is the empty pattern, which is a real route.
        assert_eq!(
            django_routes(&urlconf("    path(\"\", views.index),\n")),
            [("ANY".into(), "".into(), "index".into())],
            "an app's own root has an empty pattern"
        );
    }

    /// The matcher reads URLconfs, not every Python file with a `path()` call.
    ///
    /// A false `HandlesRoute` edge is not a harmless extra row: it is what tells
    /// liveness a symbol is reached from outside the call graph, so it silences
    /// a genuinely dead symbol. `path` is an ordinary identifier, so the file
    /// must first identify itself as a URLconf.
    #[test]
    fn only_a_urlconf_has_its_path_calls_read_as_routes() {
        assert_eq!(
            django_routes("path(\"users/\", views.user_detail)\n"),
            [],
            "a bare path() call in a file that is no URLconf is not a route"
        );
        assert_eq!(
            django_routes("from django.urls import path\npath(\"x/\", views.x)\n"),
            [("ANY".into(), "x/".into(), "x".into())],
            "the import identifies a URLconf that builds no `urlpatterns` name"
        );

        // Within a URLconf, a call that merely ends in `path` is still not one.
        assert_eq!(
            django_routes(&urlconf("    os.path(\"x/\", views.x),\n")),
            [],
            "a member call is not Django's path()"
        );
        assert_eq!(
            django_routes(&urlconf("    mypath(\"x/\", views.x),\n")),
            [],
            "path must be the whole callee, not the tail of a longer name"
        );

        // The import line names `path` without calling it.
        assert_eq!(
            django_routes("from django.urls import path\nurlpatterns = []\n"),
            [],
            "importing the name registers no route"
        );
    }

    /// Django routes reach the matcher under the language the walk names.
    ///
    /// Files are extracted by language, so the Python arm is what a real
    /// urls.py arrives under; `django` is the explicit alias, matching how
    /// `fastapi` and `flask` name the arm they share.
    #[test]
    fn django_routes_dispatch_on_python_and_on_django() {
        let source = urlconf("    path(\"x/\", views.x),\n");
        for name in ["python", "django"] {
            let routes = extract_framework_routes(name, &source).expect("matcher runs");
            assert_eq!(routes.len(), 1, "{name} must extract a Django route");
            assert_eq!(routes[0].framework, "django");
        }

        // Another language's matcher must not claim a URLconf.
        for name in ["rust", "javascript", "cobol"] {
            assert!(
                extract_framework_routes(name, &source)
                    .expect("no error")
                    .is_empty(),
                "{name} must not extract a Django route"
            );
        }

        // The Python arm still runs the decorator matcher beside this one, and
        // a URLconf is not a decorator.
        let both = "from django.urls import path\n\
                    urlpatterns = [path(\"x/\", views.x)]\n\
                    @app.get('/y')\n\
                    def y():\n    return 1\n";
        let mut frameworks: Vec<String> = extract_framework_routes("python", both)
            .expect("matcher runs")
            .into_iter()
            .map(|route| route.framework)
            .collect();
        frameworks.sort();
        assert_eq!(frameworks, ["django", "fastapi/flask"]);
    }

    /// A URLconf is a list, and every entry in it is a route.
    #[test]
    fn every_entry_in_a_urlpatterns_list_is_extracted() {
        assert_eq!(
            django_routes(&urlconf(
                "    path(\"\", views.index),\n\
                 \x20   path(\"users/<int:pk>/\", views.user_detail, name=\"user-detail\"),\n\
                 \x20   re_path(r\"^legacy/(?P<pk>\\d+)/$\", views.legacy_detail),\n"
            )),
            [
                ("ANY".into(), "".into(), "index".into()),
                ("ANY".into(), "users/<int:pk>/".into(), "user_detail".into()),
                ("ANY".into(), "legacy/<pk>/".into(), "legacy_detail".into()),
            ]
        );

        // Entries on one line are separated by a comma, which is the same
        // character that separates a pattern from its view: the scan must not
        // stop at the first route.
        assert_eq!(
            django_routes(&urlconf(
                "    path(\"a/\", views.a), path(\"b/\", views.b),\n"
            ))
            .len(),
            2,
            "adjacent entries are two routes"
        );
    }

    /// A URLconf entry is routinely spread over several lines and annotated.
    ///
    /// Black formats a long `path(...)` one argument per line, and route lists
    /// are one of the places people leave notes. Prose is not structure: a
    /// comma in a comment would end the view argument before the view, and an
    /// apostrophe would open a string literal that runs to end of line — either
    /// way the route binds to nothing, or to a fragment of a sentence.
    #[test]
    fn a_multi_line_entry_is_read_past_its_comments() {
        assert_eq!(
            django_routes(&urlconf(
                "    path(\n\
                 \x20       \"users/<int:pk>/\",\n\
                 \x20       views.user_detail,\n\
                 \x20       name=\"user-detail\",\n\
                 \x20   ),\n"
            )),
            [("ANY".into(), "users/<int:pk>/".into(), "user_detail".into())],
            "the pattern and view survive one-argument-per-line formatting"
        );
        assert_eq!(
            django_routes(&urlconf(
                "    path(\n\
                 \x20       \"x/\",\n\
                 \x20       # first, second\n\
                 \x20       views.detail,\n\
                 \x20   ),\n"
            )),
            [("ANY".into(), "x/".into(), "detail".into())],
            "a comma in a comment is prose, not an argument separator"
        );
        assert_eq!(
            django_routes(&urlconf(
                "    path(\n\
                 \x20       \"x/\",\n\
                 \x20       # the user's own detail view\n\
                 \x20       views.detail,\n\
                 \x20   ),\n"
            )),
            [("ANY".into(), "x/".into(), "detail".into())],
            "an apostrophe in a comment does not open a string literal"
        );

        // A `#` inside a string is data, and must not start a comment.
        assert_eq!(
            django_routes(&urlconf(
                "    path(\"x/\", views.detail, name=\"a#b\"),\n    path(\"y/\", views.other),\n"
            )),
            [
                ("ANY".into(), "x/".into(), "detail".into()),
                ("ANY".into(), "y/".into(), "other".into())
            ],
            "a hash inside a quoted argument does not comment out the next entry"
        );
    }

    /// A call the source never closes yields no route, and costs no more than
    /// the shared scan cap allows.
    #[test]
    fn an_unclosed_urlconf_entry_records_no_route() {
        assert_eq!(
            django_routes(&urlconf("    path(\"x/\", views.detail\n")),
            [],
            "an entry whose call never closes is not a route"
        );
    }
}

#[cfg(test)]
mod express_receiver_tests {
    use super::*;

    /// `METHOD /path -> handler` for every route the Express matcher finds.
    fn routes(source: &str) -> Vec<String> {
        extract_framework_routes("javascript", source)
            .expect("matcher compiles")
            .into_iter()
            .map(|route| {
                format!(
                    "{} {} -> {}",
                    route.http_method, route.path_pattern, route.handler_name
                )
            })
            .collect()
    }

    /// A router bound to any name is still a router.
    ///
    /// The same `(app|router)` allow-list as the Python side, with the same
    /// consequence: `const api = express.Router()` — the idiom the Express
    /// router guide itself uses — extracted nothing, so every handler mounted
    /// on it lost its only inbound edge.
    #[test]
    fn extracts_routes_from_a_router_bound_to_any_name() {
        assert_eq!(
            routes("const api = express.Router();\napi.get('/users', handleUsers);\n"),
            ["GET /users -> handleUsers"]
        );
        assert_eq!(
            routes("v1.post('/users/new', createUser);\n"),
            ["POST /users/new -> createUser"]
        );
        assert_eq!(
            routes("this.router.put('/x', handlePut);\n"),
            ["PUT /x -> handlePut"],
            "a class-based server reaches its router through a dotted receiver"
        );
        assert_eq!(
            routes("app.get('*', handleFallback);\n"),
            ["GET * -> handleFallback"],
            "the catch-all Express documents is a path, and its handler is real"
        );
    }

    /// What tells a route from an ordinary `X.get("string")` call.
    ///
    /// JavaScript needs a guard Python does not: `@` makes a Python decorator
    /// unmistakable, while `X.get('/thing')` is one of the most common shapes
    /// in JavaScript and would flood the route table once the receiver stopped
    /// being an allow-list. Two properties separate a route: a path that starts
    /// with `/` (or the `*` catch-all), and a second argument, because a route
    /// with no handler is not a route.
    ///
    #[test]
    fn a_route_is_told_from_a_get_call_by_its_path_and_its_handler() {
        for (source, why) in [
            (
                "axios.get('/api/users');\n",
                "an HTTP client call names no handler",
            ),
            (
                "redis.get('session:1', loadSession);\n",
                "a cache key is not a route path, even with a callback after it",
            ),
            (
                "const id = headers.get('x-request-id', fallback);\n",
                "a header name is not a route path, even with a second argument",
            ),
            (
                "cache.get('users', () => load());\n",
                "a cache lookup with a callback is not a route",
            ),
        ] {
            assert!(routes(source).is_empty(), "{why}: {:?}", routes(source));
        }
    }

    /// `app.get(name)` with one argument is Express's settings getter.
    ///
    /// This is a *separate* defect from the receiver allow-list, and predates
    /// it: `app` was already on that list, so `app.get('view engine')` matched,
    /// and the site was pushed anyway because an absent handler became `""`
    /// rather than a reason to reject it. `devmap routes --json` listed a route
    /// `GET view engine` with `normalized_path: "/view engine"` and an empty
    /// handler list — reproduced against the pre-change release binary.
    ///
    /// It gets its own test because the requirement that closes it — a second
    /// argument — is not scaffolding for widening the receiver, and must not be
    /// dropped by whoever next touches the receiver logic.
    ///
    /// The harm is a phantom row in `routes`, `api-impact` and `shape-check`
    /// and a route node binding nothing — *not* a wrongly exempted dead symbol.
    /// With an empty handler name the resolver has no symbol to bind, so the
    /// node dangles rather than producing a live `HandlesRoute` edge to
    /// something real.
    #[test]
    fn a_settings_getter_is_not_a_route() {
        assert!(
            routes("const engine = app.get('view engine');\n").is_empty(),
            "a one-argument app.get reads a setting and registers no route: {:?}",
            routes("const engine = app.get('view engine');\n")
        );
        assert_eq!(
            routes("app.set('view engine', 'pug');\napp.get('/x', handleX);\n"),
            ["GET /x -> handleX"],
            "the real route beside a settings call is still extracted"
        );
    }

    /// An HTTP client call with a config argument is not a route.
    ///
    /// This is the case the path test and the second-argument test both let
    /// through: `axios.get('/api/users', {headers})` has a `/` path and a
    /// second argument, and once the receiver stopped being an allow-list there
    /// was nothing left to tell it from `app.get('/api/users', handler)`.
    ///
    /// Two properties settle it, neither of them a list of names. Express's
    /// signature is `app.METHOD(path, ...callbacks)` — every argument after the
    /// path is a callback and the framework has no form taking trailing options
    /// — so an object literal in the final position is some other API. And an
    /// Express router is constructed, never imported ready-made, so a receiver
    /// this file binds to a bare package specifier is not a router whatever it
    /// was named.
    ///
    /// The second property is what makes an *identifier* config decidable:
    /// `axios.get(url, config)` is shaped exactly like a route, and only the
    /// binding of `axios` says otherwise.
    #[test]
    fn an_http_client_call_is_not_a_route() {
        for (source, why) in [
            (
                "app.get('/api/users', {headers});\n",
                "an options object is data; Express takes only callbacks after the path",
            ),
            (
                "client.get('/api/users', { params: { page: 1 } });\n",
                "a nested options object is still an options object",
            ),
            (
                "const axios = require('axios');\naxios.get('/api/users', config);\n",
                "an identifier config is shaped like a handler; the require says it is not",
            ),
            (
                "import axios from 'axios';\naxios.get('/api/users', config);\n",
                "the ESM default import binds the same evidence",
            ),
            (
                "import * as ky from 'ky';\nky.get('/api/users', options);\n",
                "a namespace import binds it too",
            ),
            (
                "const axios = require('axios');\nconst client = axios.create({});\n\
                 client.get('/api/users', config);\n",
                "a client built by a factory is no more a router than its factory",
            ),
            (
                "const got = require('got');\ngot.get('/x', opts);\n",
                "no client is named in the matcher; the binding is read from the file",
            ),
        ] {
            assert!(routes(source).is_empty(), "{why}: {:?}", routes(source));
        }
    }

    /// The client guards must not cost a single real route.
    ///
    /// Both new tests reject, so both can be satisfied by rejecting too much.
    /// A router is *constructed*, and every construction form has to survive —
    /// including in the file that also imports a client, which is the ordinary
    /// shape of a server that makes outbound calls.
    #[test]
    fn a_router_beside_an_http_client_still_registers_its_routes() {
        assert_eq!(
            routes(
                "const express = require('express');\nconst axios = require('axios');\n\
                 const app = express();\nconst api = express.Router();\n\
                 app.get('/health', health);\napi.get('/users', listUsers);\n\
                 axios.get('/upstream', config);\n"
            ),
            ["GET /health -> health", "GET /users -> listUsers"],
            "the client's call drops out and both real routes stay"
        );
        assert_eq!(
            routes(
                "const usersRouter = require('./routes/users');\nusersRouter.get('/x', handleX);\n"
            ),
            ["GET /x -> handleX"],
            "a relative import may export a router, so it is not treated as a client"
        );
        assert_eq!(
            routes(
                "import express from 'express';\nconst app = express();\napp.get('/x', handleX);\n"
            ),
            ["GET /x -> handleX"],
            "express is the router's own source and never a rival to it"
        );
        assert_eq!(
            routes("const router = require('express').Router();\nrouter.get('/x', handleX);\n"),
            ["GET /x -> handleX"],
            "a suffixed require constructs a router rather than binding a module"
        );
        assert_eq!(
            routes("app.get('/x', asyncHandler(getUsers));\n"),
            ["GET /x -> "],
            "a wrapper call returns a handler; it is callable, not data"
        );
        assert_eq!(
            routes("app.get('/x', [auth, log]);\n"),
            ["GET /x -> "],
            "Express documents an array of callbacks as the final argument"
        );
    }

    /// A path Express itself would never route is not a route.
    ///
    /// The `/`-or-`*` test is the JS side's only discriminator once the
    /// receiver stops being an allow-list, so it is worth stating what it
    /// costs. Express matches a request's `req.path`, which always begins with
    /// `/`, so a *string* path without one routes nothing — there is no real
    /// route this turns away. A RegExp path — `app.get(/^\/x/, h)` — is not a
    /// string literal, so neither this matcher nor the one it replaces ever saw
    /// it: that gap is unchanged, not newly introduced.
    #[test]
    fn a_regexp_path_is_out_of_reach_of_a_string_literal_matcher() {
        assert!(
            routes("app.get(/^\\/users/, handleUsers);\n").is_empty(),
            "a RegExp path is not a string literal, before or after this change"
        );
    }
}

#[cfg(test)]
mod middleware_tests {
    use super::*;

    /// `VERB path -> handler [scope:name(qualifier)...]` for every route, so a
    /// whole file's middleware reads as one comparable value. An entry that
    /// names no symbol prints its expression in braces.
    fn shape(language: &str, source: &str) -> Vec<String> {
        extract_framework_routes(language, source)
            .expect("matchers compile")
            .into_iter()
            .map(|route| {
                let middleware = match &route.middleware {
                    None => "unknown".to_string(),
                    Some(entries) => entries
                        .iter()
                        .map(|entry| {
                            let what = if entry.name.is_empty() {
                                format!("{{{}}}", entry.expression)
                            } else {
                                match &entry.qualifier {
                                    Some(q) => format!("{}({q})", entry.name),
                                    None => entry.name.clone(),
                                }
                            };
                            format!("{}:{what}", entry.scope.label())
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                };
                format!(
                    "{} {} -> {} [{middleware}]",
                    route.http_method, route.path_pattern, route.handler_name
                )
            })
            .collect()
    }

    /// The Express stack, as Express runs it: what the router registered
    /// before the route, then the route's own arguments between path and
    /// handler. A `use` after a route does not run for it, a `use` on another
    /// router does not run for it, and a mount path scopes a `use` to the
    /// paths under it — on segment boundaries, so `/api` does not cover
    /// `/apix`.
    #[test]
    fn an_express_route_carries_the_middleware_that_runs_before_it() {
        let source = concat!(
            "const express = require('express');\n",
            "import cors from 'cors';\n",
            "const app = express();\n",
            "const router = express.Router();\n",
            "app.get('/early', early);\n",
            "app.use(cors());\n",
            "app.use(express.json());\n",
            "app.use('/api', apiAuth);\n",
            "router.use(routerOnly);\n",
            "app.get('/api/users', validate, [audit, auth.required], listUsers);\n",
            "app.get('/apix', h1);\n",
            "app.get('/health', (req, res, next) => next(), health);\n",
            "app.post('/api', requireRole('admin'), function create(req, res) {});\n",
        );
        assert_eq!(
            shape("javascript", source),
            [
                "GET /early -> early []",
                "GET /api/users -> listUsers [router:cors router:json(express) router:apiAuth \
                 route:validate route:audit route:required(auth)]",
                "GET /apix -> h1 [router:cors router:json(express)]",
                "GET /health -> health [router:cors router:json(express) \
                 route:{(req, res, next) => next()}]",
                "POST /api -> create [router:cors router:json(express) router:apiAuth \
                 route:requireRole]",
            ]
        );
    }

    /// A middleware producer ran and found nothing is `Some([])`; no producer
    /// is `None`. The two must never look alike, or "this route is
    /// unauthenticated" is read off a framework nothing inspected.
    #[test]
    fn only_a_framework_with_a_producer_claims_to_know_its_middleware() {
        assert_eq!(
            shape("javascript", "app.get('/x', handler);\n"),
            ["GET /x -> handler []"]
        );
        let flask = "@app.get(\"/x\")\ndef view():\n    return 1\n";
        assert_eq!(shape("python", flask), ["GET /x -> view [unknown]"]);
        let axum = r#".route("/x", get(handler))"#;
        assert_eq!(shape("rust", axum), ["GET /x -> handler [unknown]"]);
    }

    /// chi, as written: `r.Use` before routes, a `Group` closure whose own
    /// `Use` must stay inside it, and a `With` chain on one route.
    ///
    /// The closure is the case that matters. Its parameter is also called `r`,
    /// so a name-only rule would let `requireAuth` leak onto `/after` and
    /// report a public route as authenticated. The parent's `Logger` does run
    /// inside the group — chi copies the stack — and is reported there.
    #[test]
    fn a_chi_group_scopes_its_middleware_to_its_own_block() {
        let source = r#"package api

import (
	"net/http"

	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
)

func Routes() http.Handler {
	r := chi.NewRouter()
	r.Use(middleware.Logger)
	r.Get("/public", publicHandler)
	r.Group(func(r chi.Router) {
		r.Use(requireAuth)
		r.Get("/private", h.Private)
	})
	r.With(rateLimit(10)).Post("/login", loginHandler)
	r.Get("/after", afterHandler)
	return r
}
"#;
        assert_eq!(
            shape("go", source),
            [
                "GET /public -> publicHandler [router:Logger(middleware)]",
                "GET /private -> Private [router:Logger(middleware) router:requireAuth]",
                "POST /login -> loginHandler [router:Logger(middleware) route:rateLimit]",
                "GET /after -> afterHandler [router:Logger(middleware)]",
            ]
        );
        let routes = extract_framework_routes("go", source).unwrap();
        assert!(routes.iter().all(|route| route.framework == "chi"));
    }

    /// gin: middleware before the handler, a `Group` that inherits the
    /// parent's stack and adds its own, and a handler written as a literal.
    #[test]
    fn a_gin_group_inherits_its_parents_middleware() {
        let source = r#"package server

import "github.com/gin-gonic/gin"

func Setup() *gin.Engine {
	r := gin.New()
	r.Use(gin.Recovery())
	v1 := r.Group("/v1", authRequired)
	v1.GET("/users", rateLimit, listUsers)
	r.GET("/ping", func(c *gin.Context) { c.String(200, "pong") })
	return r
}
"#;
        assert_eq!(
            shape("go", source),
            [
                "GET /users -> listUsers [router:Recovery(gin) router:authRequired \
                 route:rateLimit]",
                "GET /ping ->  [router:Recovery(gin)]",
            ]
        );
    }

    /// echo puts the handler first and route middleware after it; reading it
    /// in gin's order would name the middleware as the handler.
    #[test]
    fn echo_route_middleware_follows_the_handler() {
        let source = "package s\n\nimport \"github.com/labstack/echo/v4\"\n\n\
                      func S(e *echo.Echo) {\n\te.GET(\"/x\", getX, audit, cache.Wrap)\n}\n";
        assert_eq!(
            shape("go", source),
            ["GET /x -> getX [route:audit route:Wrap(cache)]"]
        );
    }

    /// net/http and gorilla/mux: Go 1.22 method patterns, a pattern with no
    /// method, gorilla's chained `.Methods(...)`, and an `http.Handler` value.
    #[test]
    fn net_http_and_gorilla_patterns_name_their_methods() {
        let source = r#"package web

import (
	"net/http"

	"github.com/gorilla/mux"
)

func Wire(mux *http.ServeMux, r *mux.Router) {
	mux.HandleFunc("GET /items/{id}", getItem)
	http.HandleFunc("/health", health)
	mux.Handle("/static/", &fileServer{})
	r.HandleFunc("/users", users).Methods("GET", "POST")
}
"#;
        assert_eq!(
            shape("go", source),
            [
                "GET /items/{id} -> getItem []",
                "ANY /health -> health []",
                "ANY /static/ ->  []",
                "GET /users -> users []",
                "POST /users -> users []",
            ]
        );
    }

    /// Go's other `Get`s are not routes: an HTTP client's takes one argument,
    /// a cache's takes an out-parameter, and a key that is not a path is not a
    /// route whatever follows it.
    #[test]
    fn a_go_get_that_is_not_a_route_is_not_recorded() {
        let source = r#"package c

func f() {
	resp, err := http.Get("/x")
	cache.Get("/k", &out)
	store.Get("/k", nil)
	cfg.Get("key", handler)
	r.Get("/real", handler)
}
"#;
        assert_eq!(shape("go", source), ["GET /real -> handler []"]);
    }
}
