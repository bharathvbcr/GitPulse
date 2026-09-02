use regex::Regex;
use std::sync::OnceLock;

use crate::model::*;

fn fastapi_re() -> Result<&'static Regex, String> {
    static RE: OnceLock<Result<Regex, String>> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?m)@(app|router|api)\.(get|post|put|delete|patch|options|head)\s*\(\s*["']([^"']+)["']"#,
        )
        .map_err(|error| format!("invalid FastAPI route matcher: {error}"))
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

/// Handler name from an Express route's argument list.
///
/// `app.get("/x", handleUsers)` names its handler in the final argument, after
/// any middleware. The argument may be an identifier, a member expression, or a
/// function expression — named or anonymous. An anonymous handler genuinely has
/// no name, and yields `None` rather than a placeholder: a placeholder would
/// resolve to any symbol that happened to share it.
fn express_handler_name(source: &str, after: usize) -> Option<String> {
    let rest = source.get(after..)?;

    // Walk to the `)` that closes the route call, tracking nesting so an arrow
    // function's own parentheses and braces do not end the scan early.
    let mut depth = 0i32;
    let mut end = None;
    for (offset, character) in rest.char_indices() {
        match character {
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => {
                end = Some(offset);
                break;
            }
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    let arguments = &rest[..end?];

    // The handler is the last top-level argument; earlier ones are middleware.
    let mut depth = 0i32;
    let mut last = arguments;
    for (offset, character) in arguments.char_indices() {
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
        for cap in fastapi_re()?.captures_iter(source) {
            let Some(full) = cap.get(0) else {
                continue;
            };
            routes.push(ExtractedRoute {
                framework: "fastapi/flask".to_string(),
                http_method: cap[2].to_uppercase(),
                path_pattern: cap[3].to_string(),
                handler_name: python_decorated_handler(source, full.end()).unwrap_or_default(),
                span: Span {
                    start_byte: full.start(),
                    end_byte: full.end(),
                },
            });
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
