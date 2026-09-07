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

fn python_route_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)@(app|router|api)\.(get|post|put|delete|patch|options|head|route)\s*\(\s*["']([^"']+)["']"#,
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

fn express_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?m)\b(app|router)\.(get|post|put|delete|patch)\s*\(\s*["']([^"']+)["']"#)
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

/// Name of the function a Python route decorator is attached to.
///
/// `@app.get("/items")` carries no handler name of its own — the handler is the
/// `def` the decorator is applied to, which may sit several stacked decorators
/// later. Scanning forward for it is what makes a FastAPI or Flask route
/// resolvable at all; without it the route names nothing and its handler has no
/// incoming edge, so an endpoint reachable only over HTTP reads as dead.
///
/// Returns `None` when the decorator is not attached to a function, rather than
/// guessing: a route bound to the wrong symbol is worse than one bound to none.
fn python_decorated_handler(source: &str, after: usize) -> Option<String> {
    for line in source.get(after..)?.lines().skip(1) {
        let trimmed = line.trim();
        // Blank lines, comments and further stacked decorators sit between the
        // route decorator and its function.
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('@') {
            continue;
        }
        let definition = trimmed
            .strip_prefix("async def ")
            .or_else(|| trimmed.strip_prefix("def "))?;
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
fn express_handler_name(source: &str, after: usize) -> Option<String> {
    let (arguments, _) = call_arguments(source, after)?;

    // The handler is the last top-level argument; earlier ones are middleware.
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut last = arguments;
    for (offset, character) in arguments.char_indices() {
        if in_string_literal(character, &mut quote, &mut escaped) {
            continue;
        }
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => last = &arguments[offset + 1..],
            _ => {}
        }
    }
    let candidate = last.trim();

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
            for method in python_route_methods(&cap[2], call.map(|(arguments, _)| arguments)) {
                routes.push(ExtractedRoute {
                    framework: "fastapi/flask".to_string(),
                    http_method: method,
                    path_pattern: cap[3].to_string(),
                    handler_name: handler.clone(),
                    span: Span {
                        start_byte: full.start(),
                        end_byte: full.end(),
                    },
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
            });
        }
    }

    if framework_name == "javascript"
        || framework_name == "typescript"
        || framework_name == "express"
    {
        for cap in express_re()?.captures_iter(source) {
            let Some(full) = cap.get(0) else {
                continue;
            };
            routes.push(ExtractedRoute {
                framework: "express".to_string(),
                http_method: cap[2].to_uppercase(),
                path_pattern: cap[3].to_string(),
                handler_name: express_handler_name(source, full.end()).unwrap_or_default(),
                span: Span {
                    start_byte: full.start(),
                    end_byte: full.end(),
                },
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
