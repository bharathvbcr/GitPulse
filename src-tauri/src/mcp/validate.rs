//! Argument validation against a declared JSON Schema.
//!
//! MCP 2026-07-28 makes this a server obligation, not a nicety: *"Servers
//! **MUST**: Validate all tool inputs"*
//! ([tools#security-considerations](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)).
//! Before this existed, every tool read its arguments straight out of the
//! `Value` with `as_str()` / `as_u64()`, so the declared schema was decoration:
//! `{"limit": "500"}` produced `None`, which fell back to the default, and the
//! caller was told nothing. A request for 500 answered with 200 and reported as
//! success is the same failure shape as a check that could not run reporting a
//! pass.
//!
//! This is a validator for the schema subset the catalog actually uses —
//! `type`, `required`, `additionalProperties`, `enum`, `minimum`, `maximum`,
//! `minLength`, `maxLength` on a flat object — not a general JSON Schema
//! engine. That is deliberate: [`schema_is_supported`] walks every advertised
//! schema and a test fails the build if the catalog ever grows a keyword this
//! file would silently ignore. An unsupported keyword must never validate as
//! if it had been checked.

use std::cmp::Ordering;

use serde_json::{json, Number, Value};

/// One violation, phrased for a language model that has to fix its own call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub path: String,
    pub message: String,
}

impl Violation {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }

    pub fn render(&self) -> String {
        if self.path.is_empty() {
            self.message.clone()
        } else {
            format!("{}: {}", self.path, self.message)
        }
    }
}

/// Keywords [`validate`] understands. Anything outside this set in an
/// advertised schema is a build failure, not a silent pass.
const SUPPORTED_KEYWORDS: &[&str] = &[
    "type",
    "description",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
    "items",
];

/// Every keyword in `schema` that [`validate`] would ignore.
///
/// The point is the honesty invariant: a schema carrying `pattern` would look
/// validated while `pattern` was never applied. The catalog test calls this and
/// fails on any non-empty result, so the two can never drift apart.
pub fn unsupported_keywords(schema: &Value) -> Vec<String> {
    let mut found = Vec::new();
    collect_unsupported(schema, "", &mut found);
    found.sort();
    found.dedup();
    found
}

fn collect_unsupported(schema: &Value, path: &str, found: &mut Vec<String>) {
    let Some(object) = schema.as_object() else {
        return;
    };
    for (key, value) in object {
        if !SUPPORTED_KEYWORDS.contains(&key.as_str()) {
            found.push(if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            });
            continue;
        }
        match key.as_str() {
            "properties" => {
                if let Some(properties) = value.as_object() {
                    for (name, sub) in properties {
                        let child = if path.is_empty() {
                            format!("properties.{name}")
                        } else {
                            format!("{path}.properties.{name}")
                        };
                        collect_unsupported(sub, &child, found);
                    }
                }
            }
            "items" => {
                let child = if path.is_empty() {
                    "items".to_string()
                } else {
                    format!("{path}.items")
                };
                collect_unsupported(value, &child, found);
            }
            _ => {}
        }
    }
}

/// Check `arguments` against `schema`, returning every violation at once.
///
/// All violations rather than the first: a model that fixes one argument per
/// round trip burns a turn per mistake.
pub fn validate(arguments: &Value, schema: &Value) -> Vec<Violation> {
    let mut violations = Vec::new();
    check(arguments, schema, "", &mut violations);
    violations
}

fn schema_allows_null(schema: &Value) -> bool {
    match schema.get("type") {
        Some(Value::String(name)) => name == "null",
        Some(Value::Array(names)) => names.iter().any(|name| name.as_str() == Some("null")),
        _ => false,
    }
}

fn check(value: &Value, schema: &Value, path: &str, out: &mut Vec<Violation>) {
    let Some(schema) = schema.as_object() else {
        return;
    };

    if let Some(expected) = schema.get("type") {
        let matches = match expected {
            Value::String(name) => type_matches(value, name),
            Value::Array(names) if !names.is_empty() => {
                names.iter().try_fold(false, |matched, name| {
                    type_matches(value, name.as_str()?).map(|member| matched || member)
                })
            }
            _ => None,
        };
        if matches != Some(true) {
            out.push(Violation::new(
                path,
                format!(
                    "expected {}, got {}",
                    render_scalar(expected),
                    describe(value)
                ),
            ));
            // Every other keyword is typed; checking them against the wrong
            // kind of value would only produce noise on top of the real cause.
            return;
        }
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            let rendered: Vec<String> = allowed.iter().map(render_scalar).collect();
            out.push(Violation::new(
                path,
                format!("must be one of [{}]", rendered.join(", ")),
            ));
        }
    }

    match value {
        Value::Object(fields) => {
            let properties = schema.get("properties").and_then(Value::as_object);

            for required in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                // An explicit JSON `null` is absence for string/number
                // arguments. Output fields whose type includes `null` (for
                // example `fresh: null` meaning unverified) are present values.
                match fields.get(required) {
                    None => out.push(Violation::new(
                        join(path, required),
                        "required argument is missing",
                    )),
                    Some(Value::Null) => {
                        let allows_null = properties
                            .and_then(|p| p.get(required))
                            .is_some_and(schema_allows_null);
                        if !allows_null {
                            out.push(Violation::new(
                                join(path, required),
                                "required argument is missing",
                            ));
                        }
                    }
                    Some(_) => {}
                }
            }

            let closed = schema
                .get("additionalProperties")
                .and_then(Value::as_bool)
                .is_some_and(|open| !open);
            if closed {
                for name in fields.keys() {
                    if properties.is_none_or(|p| !p.contains_key(name)) {
                        let known: Vec<&str> = properties
                            .map(|p| p.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        out.push(Violation::new(
                            join(path, name),
                            format!("{UNKNOWN_PREFIX}{}]", known.join(", ")),
                        ));
                    }
                }
            }

            if let Some(properties) = properties {
                for (name, sub) in properties {
                    match fields.get(name) {
                        // Absent optional arguments are the normal case. An
                        // explicit null is absence unless the field's type
                        // includes `null`, in which case it is a typed value.
                        None => {}
                        Some(Value::Null) if !schema_allows_null(sub) => {}
                        Some(present) => check(present, sub, &join(path, name), out),
                    }
                }
            }
        }
        Value::Array(items) => {
            let count = items.len() as u64;
            if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
                if count < min {
                    out.push(Violation::new(
                        path,
                        format!("must have at least {min} items, got {count}"),
                    ));
                }
            }
            if let Some(max) = schema.get("maxItems").and_then(Value::as_u64) {
                if count > max {
                    out.push(Violation::new(
                        path,
                        format!("must have at most {max} items, got {count}"),
                    ));
                }
            }
            if let Some(sub) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    check(item, sub, &format!("{path}[{index}]"), out);
                }
            }
        }
        Value::String(text) => {
            let len = text.chars().count() as u64;
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
                if len < min {
                    out.push(Violation::new(
                        path,
                        format!("must be at least {min} characters, got {len}"),
                    ));
                }
            }
            if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
                if len > max {
                    out.push(Violation::new(
                        path,
                        format!("must be at most {max} characters, got {len}"),
                    ));
                }
            }
        }
        Value::Number(actual) => {
            if let Some(min) = schema.get("minimum").and_then(Value::as_number) {
                if order(actual, min).is_some_and(Ordering::is_lt) {
                    out.push(Violation::new(path, format!("must be >= {}", bound(min))));
                }
            }
            if let Some(max) = schema.get("maximum").and_then(Value::as_number) {
                if order(actual, max).is_some_and(Ordering::is_gt) {
                    out.push(Violation::new(path, format!("must be <= {}", bound(max))));
                }
            }
        }
        _ => {}
    }
}

fn integer_literal(number: &Number) -> Option<i128> {
    number
        .as_i64()
        .map(i128::from)
        .or_else(|| number.as_u64().map(i128::from))
}

/// How `actual` compares with a declared bound. Two integers compare as
/// integers: through `f64`, `9223372036854775808` equals the bound
/// `9223372036854775807` (both round to 2^63), so a value one past an `i64`
/// maximum passed a check that existed to stop it.
fn order(actual: &Number, bound: &Number) -> Option<Ordering> {
    match (integer_literal(actual), integer_literal(bound)) {
        (Some(actual), Some(bound)) => Some(actual.cmp(&bound)),
        _ => actual.as_f64()?.partial_cmp(&bound.as_f64()?),
    }
}

fn bound(number: &Number) -> String {
    match integer_literal(number) {
        Some(integer) => integer.to_string(),
        None => number.as_f64().map(|n| n.to_string()).unwrap_or_default(),
    }
}

/// JSON Schema `type` against a concrete value.
///
/// `integer` is the one that has to be spelled out: JSON has a single number
/// type, so `1.0` is an integer and `1.5` is not, and serde_json's `is_i64`
/// answers "was it written without a decimal point", which is a different
/// question. The answer is [`exact_int`]'s, so a value this accepts is a value
/// extraction can convert: a validator that called `1e300` an integer would
/// pass what [`FromArg`] has to refuse.
fn type_matches(value: &Value, expected: &str) -> Option<bool> {
    Some(match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value
            .as_number()
            .is_some_and(|number| exact_int(number).is_some()),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        // An unknown type keyword must not silently pass. `unsupported_keywords`
        // cannot catch this (the keyword itself is supported, its value is not),
        // so refusing here is what keeps it honest.
        _ => return None,
    })
}

fn describe(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Number(n) => {
            if exact_int(n).is_some() {
                "integer".into()
            } else if n.as_f64().is_some_and(|f| f.abs() > MAX_EXACT_FLOAT_INT) {
                "float above 999999999999999; send an integer literal".into()
            } else {
                "number".into()
            }
        }
        Value::String(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(_) => "object".into(),
    }
}

fn render_scalar(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}.{name}")
    }
}

// ---------------------------------------------------------------------------
// Typed arguments
//
// One declaration per tool produces both the advertised `inputSchema` and the
// extraction the handler reads, so the two cannot drift: a field exists in the
// schema if and only if it exists on the struct. `validate` still owns the
// bounds (`minimum`, `maximum`, `minLength`, `maxLength`, `enum`) by reading the
// schema these declarations print. Extraction owns only what a schema cannot say
// about Rust: the JSON type, whether an integer is exact, whether it fits the
// target width, and a byte bound.
//
// Serde is not used for this on purpose. It rejects `5.0` for a `u64`, which the
// validator accepts as an integer; it stops at the first error, where a model
// fixing its own call wants every one; and a `#[serde(default)]` field rejects
// the explicit `null` the validator treats as absence.
// ---------------------------------------------------------------------------

/// What a length bound counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// Unicode scalar values, which is what `maxLength` means in JSON Schema.
    Chars,
    /// UTF-8 bytes. The schema still says `maxLength: N` (a string of N bytes
    /// has at most N characters), and extraction enforces the byte count.
    Bytes,
}

/// The longest string an `enum` argument is declared with.
const ENUM_MAX_LEN: usize = 32;

/// One argument's declaration: what the schema says, and what extraction
/// checks beyond it.
#[derive(Debug, Clone, Copy)]
pub enum Spec {
    Str {
        desc: &'static str,
        min: usize,
        max: usize,
        unit: Unit,
    },
    Enum {
        desc: &'static str,
        values: &'static [&'static str],
    },
    Int {
        desc: &'static str,
        min: i64,
        max: u64,
    },
    Num {
        desc: &'static str,
        min: f64,
        max: f64,
    },
    Bool {
        desc: &'static str,
    },
    StrArray {
        desc: &'static str,
        min_items: usize,
        max_items: usize,
        item_min: usize,
        item_max: usize,
        unit: Unit,
    },
    ObjArray {
        desc: &'static str,
        min_items: usize,
        max_items: usize,
        item: fn() -> Value,
    },
}

impl Spec {
    /// A string bounded in characters.
    pub const fn chars(desc: &'static str, min: usize, max: usize) -> Self {
        Self::Str {
            desc,
            min,
            max,
            unit: Unit::Chars,
        }
    }

    /// A string bounded in UTF-8 bytes.
    pub const fn bytes(desc: &'static str, min: usize, max: usize) -> Self {
        Self::Str {
            desc,
            min,
            max,
            unit: Unit::Bytes,
        }
    }

    /// An array of strings, each bounded in UTF-8 bytes.
    pub const fn bytes_array(desc: &'static str, max_items: usize, item_max: usize) -> Self {
        Self::StrArray {
            desc,
            min_items: 0,
            max_items,
            item_min: 0,
            item_max,
            unit: Unit::Bytes,
        }
    }

    /// An array of strings, each bounded in characters.
    pub const fn chars_array(desc: &'static str, max_items: usize, item_max: usize) -> Self {
        Self::StrArray {
            desc,
            min_items: 0,
            max_items,
            item_min: 0,
            item_max,
            unit: Unit::Chars,
        }
    }

    pub const fn int(desc: &'static str, min: i64, max: u64) -> Self {
        Self::Int { desc, min, max }
    }

    pub const fn flag(desc: &'static str) -> Self {
        Self::Bool { desc }
    }

    pub const fn one_of(desc: &'static str, values: &'static [&'static str]) -> Self {
        Self::Enum { desc, values }
    }

    /// The JSON Schema fragment for this argument.
    pub fn schema(&self) -> Value {
        match *self {
            Self::Str {
                desc,
                min,
                max,
                unit,
            } => {
                let mut schema = json!({
                    "type": "string",
                    "description": format!("{desc} {}", bound_note(max, unit, false)),
                    "maxLength": max
                });
                if min > 0 {
                    schema["minLength"] = json!(min);
                }
                schema
            }
            Self::Enum { desc, values } => json!({
                "type": "string",
                "enum": values,
                "maxLength": ENUM_MAX_LEN,
                "description": desc
            }),
            Self::Int { desc, min, max } => json!({
                "type": "integer",
                "description": desc,
                "minimum": min,
                "maximum": max
            }),
            Self::Num { desc, min, max } => json!({
                "type": "number",
                "description": desc,
                "minimum": min,
                "maximum": max
            }),
            Self::Bool { desc } => json!({ "type": "boolean", "description": desc }),
            Self::StrArray {
                desc,
                min_items,
                max_items,
                item_min,
                item_max,
                unit,
            } => {
                let mut item = json!({ "type": "string", "maxLength": item_max });
                if item_min > 0 {
                    item["minLength"] = json!(item_min);
                }
                let mut schema = json!({
                    "type": "array",
                    "description": format!("{desc} {}", bound_note(item_max, unit, true)),
                    "items": item,
                    "maxItems": max_items
                });
                if min_items > 0 {
                    schema["minItems"] = json!(min_items);
                }
                schema
            }
            Self::ObjArray {
                desc,
                min_items,
                max_items,
                item,
            } => {
                let mut schema = json!({
                    "type": "array",
                    "description": desc,
                    "items": item(),
                    "maxItems": max_items
                });
                if min_items > 0 {
                    schema["minItems"] = json!(min_items);
                }
                schema
            }
        }
    }
}

/// The sentence fragment that says what a length bound counts. A client reading
/// only the schema cannot otherwise tell `maxLength: 128` characters from 128
/// bytes, and a label of 128 three-byte characters is one or the other's limit.
fn bound_note(max: usize, unit: Unit, each_item: bool) -> String {
    let each = if each_item { "each item " } else { "" };
    match unit {
        Unit::Chars => format!("({each}at most {max} characters)"),
        Unit::Bytes => format!("({each}at most {max} bytes, UTF-8)"),
    }
}

/// The largest whole number a float-typed JSON number is trusted to mean:
/// 10^15 - 1. serde_json is built without `float_roundtrip`, so a float with 16
/// or more significant digits (`9007199254740991.0`) can parse one ulp off the
/// number the client wrote, and converting it would silently name a different
/// integer. Up to 15 significant digits every spelling takes the correctly
/// rounded path. Above it the caller is told to send an integer literal, which
/// is read exactly at any size in range.
const MAX_EXACT_FLOAT_INT: f64 = 999_999_999_999_999.0;

/// A JSON number as an exact integer, or `None`.
///
/// Integer literals are exact. A float with no fractional part (`5.0`, `1e2`) is
/// converted when its magnitude is at most 10^15 - 1 (see
/// [`MAX_EXACT_FLOAT_INT`]); beyond that it is refused rather than misread.
/// Everything else is not an integer.
pub fn exact_int(number: &serde_json::Number) -> Option<i128> {
    if let Some(signed) = number.as_i64() {
        return Some(signed.into());
    }
    if let Some(unsigned) = number.as_u64() {
        return Some(unsigned.into());
    }
    let float = number.as_f64()?;
    (float.is_finite() && float.fract() == 0.0 && float.abs() <= MAX_EXACT_FLOAT_INT)
        .then_some(float as i128)
}

/// Read one argument out of a call the schema has accepted.
pub trait FromArg: Sized {
    /// Whether the schema lists this argument under `required`.
    const REQUIRED: bool;
    fn from_arg(value: &Value, spec: &Spec, path: &str) -> Result<Self, Violation>;
    /// The answer for a missing argument or an explicit `null`.
    fn absent(path: &str) -> Result<Self, Violation>;
}

fn missing(path: &str) -> Violation {
    Violation::new(path, "required argument is missing")
}

impl<T: FromArg> FromArg for Option<T> {
    const REQUIRED: bool = false;

    fn from_arg(value: &Value, spec: &Spec, path: &str) -> Result<Self, Violation> {
        T::from_arg(value, spec, path).map(Some)
    }

    fn absent(_: &str) -> Result<Self, Violation> {
        Ok(None)
    }
}

fn wrong_type(path: &str, expected: &str, value: &Value) -> Violation {
    Violation::new(
        path,
        format!("expected {expected}, got {}", describe(value)),
    )
}

/// A string checked against a byte bound, when its spec declares one.
fn within_bytes(text: &str, unit: Unit, max: usize, path: &str) -> Result<(), Violation> {
    if unit == Unit::Bytes && text.len() > max {
        return Err(Violation::new(
            path,
            format!("must be at most {max} bytes of UTF-8, got {}", text.len()),
        ));
    }
    Ok(())
}

impl FromArg for String {
    const REQUIRED: bool = true;

    fn from_arg(value: &Value, spec: &Spec, path: &str) -> Result<Self, Violation> {
        let text = value
            .as_str()
            .ok_or_else(|| wrong_type(path, "string", value))?;
        if let Spec::Str { unit, max, .. } = *spec {
            within_bytes(text, unit, max, path)?;
        }
        Ok(text.to_owned())
    }

    fn absent(path: &str) -> Result<Self, Violation> {
        Err(missing(path))
    }
}

impl FromArg for Vec<String> {
    const REQUIRED: bool = true;

    fn from_arg(value: &Value, spec: &Spec, path: &str) -> Result<Self, Violation> {
        let items = value
            .as_array()
            .ok_or_else(|| wrong_type(path, "array", value))?;
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let at = format!("{path}[{index}]");
                let text = item
                    .as_str()
                    .ok_or_else(|| wrong_type(&at, "string", item))?;
                if let Spec::StrArray { unit, item_max, .. } = *spec {
                    within_bytes(text, unit, item_max, &at)?;
                }
                Ok(text.to_owned())
            })
            .collect()
    }

    fn absent(path: &str) -> Result<Self, Violation> {
        Err(missing(path))
    }
}

impl FromArg for bool {
    const REQUIRED: bool = true;

    fn from_arg(value: &Value, _: &Spec, path: &str) -> Result<Self, Violation> {
        value
            .as_bool()
            .ok_or_else(|| wrong_type(path, "boolean", value))
    }

    fn absent(path: &str) -> Result<Self, Violation> {
        Err(missing(path))
    }
}

macro_rules! integer_arg {
    ($($int:ty),*) => {$(
        impl FromArg for $int {
            const REQUIRED: bool = true;

            fn from_arg(value: &Value, _: &Spec, path: &str) -> Result<Self, Violation> {
                let number = value
                    .as_number()
                    .ok_or_else(|| wrong_type(path, "integer", value))?;
                let exact = exact_int(number)
                    .ok_or_else(|| wrong_type(path, "integer", value))?;
                <$int>::try_from(exact).map_err(|_| {
                    Violation::new(path, format!("must fit in {}", stringify!($int)))
                })
            }

            fn absent(path: &str) -> Result<Self, Violation> {
                Err(missing(path))
            }
        }
    )*};
}

integer_arg!(i64, u64, u32, u8, usize);

impl FromArg for f32 {
    const REQUIRED: bool = true;

    fn from_arg(value: &Value, _: &Spec, path: &str) -> Result<Self, Violation> {
        let wide = value
            .as_f64()
            .ok_or_else(|| wrong_type(path, "number", value))?;
        let narrow = wide as f32;
        if !narrow.is_finite() {
            return Err(Violation::new(path, "must fit in f32"));
        }
        Ok(narrow)
    }

    fn absent(path: &str) -> Result<Self, Violation> {
        Err(missing(path))
    }
}

/// A closed object of arguments: one struct per tool, declared with
/// [`tool_args!`].
pub trait ToolArgs: Sized {
    /// Every argument name, in declaration order.
    const FIELDS: &'static [&'static str];
    fn properties() -> Value;
    fn required() -> Vec<&'static str>;
    /// Extract every argument, reporting every violation at once. An unknown
    /// key is a violation, and an explicit `null` is absence, as in
    /// [`validate`].
    fn from_args(arguments: &Value) -> Result<Self, Vec<Violation>>;

    /// What was extracted, as the JSON a caller would have sent. The oracle the
    /// property test holds extraction to.
    #[cfg(test)]
    fn to_value(&self) -> Value;

    /// The whole `inputSchema` object.
    fn object_schema() -> Value {
        json!({
            "type": "object",
            "properties": Self::properties(),
            "required": Self::required(),
            "additionalProperties": false
        })
    }
}

/// Read `name` from `arguments` as a `T`, recording any violation.
pub fn take<T: FromArg>(
    arguments: &serde_json::Map<String, Value>,
    name: &str,
    spec: &Spec,
    violations: &mut Vec<Violation>,
) -> Option<T> {
    let read = match arguments.get(name) {
        None | Some(Value::Null) => T::absent(name),
        Some(value) => T::from_arg(value, spec, name),
    };
    match read {
        Ok(value) => Some(value),
        Err(violation) => {
            violations.push(violation);
            None
        }
    }
}

impl<T: ToolArgs> FromArg for Vec<T> {
    const REQUIRED: bool = true;

    fn from_arg(value: &Value, _: &Spec, path: &str) -> Result<Self, Violation> {
        let items = value
            .as_array()
            .ok_or_else(|| wrong_type(path, "array", value))?;
        // The first bad element is reported; `validate` has already listed every
        // one, so this only fires for a call that bypassed it.
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                T::from_args(item).map_err(|violations| {
                    let at = format!("{path}[{index}]");
                    match violations.first() {
                        Some(violation) => {
                            Violation::new(join(&at, &violation.path), &violation.message)
                        }
                        None => Violation::new(at, "invalid element"),
                    }
                })
            })
            .collect()
    }

    fn absent(path: &str) -> Result<Self, Violation> {
        Err(missing(path))
    }
}

/// Declare a tool's arguments once.
///
/// ```ignore
/// tool_args! { struct StatusArgs { repo_path: String = repo_spec() } }
/// ```
///
/// A field of type `Option<T>` is optional; every other field is `required`.
/// The macro prints the schema from the same `Spec`s that drive extraction.
macro_rules! tool_args {
    ($(#[$meta:meta])* $vis:vis struct $name:ident { $($field:ident : $ty:ty = $spec:expr),+ $(,)? }) => {
        $(#[$meta])*
        #[cfg_attr(test, derive(serde::Serialize))]
        $vis struct $name {
            $($field: $ty),+
        }

        impl $crate::mcp::validate::ToolArgs for $name {
            const FIELDS: &'static [&'static str] = &[$(stringify!($field)),+];

            fn properties() -> serde_json::Value {
                let mut properties = serde_json::Map::new();
                $(properties.insert(stringify!($field).to_string(), $spec.schema());)+
                serde_json::Value::Object(properties)
            }

            fn required() -> Vec<&'static str> {
                let mut required = Vec::new();
                $(
                    if <$ty as $crate::mcp::validate::FromArg>::REQUIRED {
                        required.push(stringify!($field));
                    }
                )+
                required
            }

            fn from_args(
                arguments: &serde_json::Value,
            ) -> Result<Self, Vec<$crate::mcp::validate::Violation>> {
                let Some(object) = arguments.as_object() else {
                    return Err(vec![$crate::mcp::validate::not_an_object(arguments)]);
                };
                let mut violations = $crate::mcp::validate::unknown_arguments(object, Self::FIELDS);
                $(
                    let $field = $crate::mcp::validate::take::<$ty>(
                        object,
                        stringify!($field),
                        &$spec,
                        &mut violations,
                    );
                )+
                match ($($field,)+) {
                    ($(Some($field),)+) if violations.is_empty() => Ok(Self { $($field),+ }),
                    _ => Err(violations),
                }
            }

            #[cfg(test)]
            fn to_value(&self) -> serde_json::Value {
                serde_json::to_value(self).expect("arguments serialize")
            }
        }
    };
}

pub(crate) use tool_args;

pub fn not_an_object(arguments: &Value) -> Violation {
    wrong_type("", "object", arguments)
}

/// A violation for each key of `arguments` that is not in `known`.
pub fn unknown_arguments(
    arguments: &serde_json::Map<String, Value>,
    known: &[&str],
) -> Vec<Violation> {
    arguments
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .map(|key| {
            Violation::new(
                key.as_str(),
                format!("{UNKNOWN_PREFIX}{}]", known.join(", ")),
            )
        })
        .collect()
}

/// The start of an unknown-argument violation's message, up to the accepted names.
const UNKNOWN_PREFIX: &str = "unknown argument; accepted: [";

/// How many violations a refusal spells out; the rest are counted.
const MAX_RENDERED_VIOLATIONS: usize = 20;

/// The longest refusal text. A call with 100,000 unknown keys used to make a
/// 6.5 MB message that the output bound dropped, so the client got no answer.
pub const MAX_REFUSAL_BYTES: usize = 8 * 1024;

/// The refusal text for a call that failed validation or extraction.
///
/// At most [`MAX_RENDERED_VIOLATIONS`] are spelled out, then "... and N more";
/// the accepted-names list of an unknown-argument violation is printed once, not
/// once per key; and the whole text is bounded by [`MAX_REFUSAL_BYTES`].
pub fn refusal(tool: &str, violations: &[Violation]) -> String {
    let mut accepted: Option<&str> = None;
    let spelled: Vec<String> = violations
        .iter()
        .take(MAX_RENDERED_VIOLATIONS)
        .map(
            |violation| match violation.message.strip_prefix(UNKNOWN_PREFIX) {
                Some(names) => {
                    accepted.get_or_insert(names);
                    format!("{}: unknown argument", violation.path)
                }
                None => violation.render(),
            },
        )
        .collect();
    let mut message = format!("invalid arguments for {tool}: {}", spelled.join("; "));
    if let Some(names) = accepted {
        message.push_str(&format!("; accepted: [{names}"));
    }
    if violations.len() > MAX_RENDERED_VIOLATIONS {
        message.push_str(&format!(
            "; \u{2026} and {} more",
            violations.len() - MAX_RENDERED_VIOLATIONS
        ));
    }
    if message.len() > MAX_REFUSAL_BYTES {
        let mut end = MAX_REFUSAL_BYTES - '\u{2026}'.len_utf8();
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push('\u{2026}');
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo_path": { "type": "string", "minLength": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500 },
                "mode": { "type": "string", "enum": ["fast", "full"] }
            },
            "required": ["repo_path"],
            "additionalProperties": false
        })
    }

    fn messages(arguments: Value) -> Vec<String> {
        validate(&arguments, &schema())
            .into_iter()
            .map(|v| v.render())
            .collect()
    }

    #[test]
    fn a_valid_call_has_no_violations() {
        assert!(messages(json!({ "repo_path": "/tmp/x", "limit": 10, "mode": "fast" })).is_empty());
    }

    #[test]
    fn nullable_types_accept_only_the_declared_alternatives() {
        let schema = json!({"type": ["boolean", "null"]});
        for value in [json!(true), json!(false), Value::Null] {
            assert!(validate(&value, &schema).is_empty());
        }
        for value in [json!("true"), json!(1), json!([]), json!({})] {
            assert_eq!(validate(&value, &schema).len(), 1, "{value}");
        }
    }

    #[test]
    fn malformed_type_declarations_never_disable_validation() {
        for declaration in [
            json!([]),
            json!(42),
            Value::Null,
            json!(["string", 42]),
            json!(["strong"]),
            json!(["string", "strong"]),
        ] {
            assert!(!validate(&json!("x"), &json!({"type": declaration})).is_empty());
        }
    }

    #[test]
    fn a_missing_required_argument_is_named() {
        let found = messages(json!({ "limit": 10 }));
        assert_eq!(found, vec!["repo_path: required argument is missing"]);
    }

    #[test]
    fn a_string_where_a_number_belongs_is_refused_not_defaulted() {
        // The regression this whole module exists for: `"500"` used to reach
        // `as_u64()`, return None, and silently become the default.
        let found = messages(json!({ "repo_path": "/tmp/x", "limit": "500" }));
        assert_eq!(found, vec!["limit: expected integer, got string"]);
    }

    #[test]
    fn a_fractional_number_is_not_an_integer() {
        let found = messages(json!({ "repo_path": "/tmp/x", "limit": 1.5 }));
        assert_eq!(found, vec!["limit: expected integer, got number"]);
    }

    #[test]
    fn a_whole_float_is_an_integer() {
        assert!(messages(json!({ "repo_path": "/tmp/x", "limit": 10.0 })).is_empty());
    }

    #[test]
    fn bounds_are_enforced_in_both_directions() {
        assert_eq!(
            messages(json!({ "repo_path": "/tmp/x", "limit": 0 })),
            vec!["limit: must be >= 1"]
        );
        assert_eq!(
            messages(json!({ "repo_path": "/tmp/x", "limit": 100_000 })),
            vec!["limit: must be <= 500"]
        );
        // Negative used to be indistinguishable from "not provided".
        assert_eq!(
            messages(json!({ "repo_path": "/tmp/x", "limit": -1 })),
            vec!["limit: must be >= 1"]
        );
    }

    #[test]
    fn an_empty_string_fails_min_length() {
        assert_eq!(
            messages(json!({ "repo_path": "" })),
            vec!["repo_path: must be at least 1 characters, got 0"]
        );
    }

    #[test]
    fn a_closed_schema_names_the_arguments_it_accepts() {
        let found = messages(json!({ "repo_path": "/tmp/x", "reppo_path": "/tmp/y" }));
        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("reppo_path: unknown argument; accepted: ["));
        assert!(found[0].contains("repo_path"));
    }

    #[test]
    fn enum_violations_list_the_allowed_values() {
        assert_eq!(
            messages(json!({ "repo_path": "/tmp/x", "mode": "turbo" })),
            vec!["mode: must be one of [fast, full]"]
        );
    }

    #[test]
    fn every_violation_is_reported_not_just_the_first() {
        let found = messages(json!({ "limit": "x", "mode": "turbo" }));
        assert_eq!(found.len(), 3, "{found:?}");
    }

    #[test]
    fn an_explicit_null_is_absence_for_required_and_optional_alike() {
        assert_eq!(
            messages(json!({ "repo_path": null })),
            vec!["repo_path: required argument is missing"]
        );
        assert!(messages(json!({ "repo_path": "/tmp/x", "limit": null })).is_empty());
    }

    #[test]
    fn a_required_field_whose_type_includes_null_accepts_explicit_null() {
        // Codeintel output advertises `fresh: null` as "the check did not
        // run". Treating that null as absence made every unverified envelope
        // fail its own schema.
        let schema = json!({
            "type": "object",
            "properties": {
                "fresh": { "type": ["boolean", "null"] },
                "reason": { "type": ["string", "null"] }
            },
            "required": ["fresh"]
        });
        assert!(validate(&json!({ "fresh": null, "reason": "unverified" }), &schema).is_empty());
        assert!(validate(&json!({ "fresh": false }), &schema).is_empty());
        assert_eq!(
            validate(&json!({ "reason": "unverified" }), &schema)
                .into_iter()
                .map(|v| v.render())
                .collect::<Vec<_>>(),
            vec!["fresh: required argument is missing"]
        );
    }

    #[test]
    fn a_non_object_argument_bag_is_a_type_violation() {
        let found = validate(&json!([1, 2, 3]), &schema());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].render(), "expected object, got array");
    }

    #[test]
    fn an_unknown_type_keyword_refuses_rather_than_passing() {
        // A typo in the catalog must fail closed. Nothing else in this file can
        // catch it: `type` is a supported keyword, so only the match arm can.
        let found = validate(&json!("x"), &json!({ "type": "strong" }));
        assert_eq!(found.len(), 1);
        assert!(found[0].render().contains("expected strong"));
    }

    #[test]
    fn unsupported_keywords_are_reported_rather_than_ignored() {
        let found = unsupported_keywords(&json!({
            "type": "object",
            "properties": { "a": { "type": "string", "pattern": "^x" } },
            "oneOf": []
        }));
        assert_eq!(found, vec!["oneOf", "properties.a.pattern"]);
    }

    #[test]
    fn a_fully_supported_schema_reports_nothing_unsupported() {
        assert!(unsupported_keywords(&schema()).is_empty());
    }

    #[test]
    fn a_whole_float_beyond_the_exact_range_is_not_an_integer() {
        // Past 2^53 - 1 a double stands for a range of integers; converting it
        // would pick one for the caller. The validator and extraction answer the
        // same question, so what passes one passes the other.
        for beyond in [json!(9_007_199_254_740_992.0), json!(1e300)] {
            let found = messages(json!({ "repo_path": "/tmp/x", "limit": beyond }));
            assert_eq!(
                found,
                vec!["limit: expected integer, got float above 999999999999999; send an integer literal"],
                "{beyond}"
            );
        }
        assert!(
            messages(json!({ "repo_path": "/tmp/x", "limit": 999_999_999_999_999.0 }))
                .iter()
                .all(|m| m.contains("<="))
        );
    }

    #[test]
    fn integer_bounds_compare_as_integers() {
        // Through f64, 2^63 equals the bound 2^63 - 1, so one past an i64
        // maximum passed the check that existed to stop it.
        let schema = json!({ "type": "integer", "maximum": i64::MAX });
        assert!(validate(&json!(i64::MAX), &schema).is_empty());
        let over = validate(&json!(9_223_372_036_854_775_808_u64), &schema);
        assert_eq!(over[0].message, "must be <= 9223372036854775807");
    }

    fn number(value: Value) -> serde_json::Number {
        match value {
            Value::Number(number) => number,
            other => panic!("not a number: {other}"),
        }
    }

    #[test]
    fn exact_int_table() {
        for (value, expected) in [
            (json!(0), Some(0)),
            (json!(-7), Some(-7)),
            (json!(i64::MIN), Some(i128::from(i64::MIN))),
            (json!(u64::MAX), Some(i128::from(u64::MAX))),
            (json!(5.0), Some(5)),
            (json!(-3.0), Some(-3)),
            (json!(-0.0), Some(0)),
            (json!(1e2), Some(100)),
            (json!(999_999_999_999_999.0), Some(999_999_999_999_999)),
            (json!(9_007_199_254_740_991.0), None),
            (json!(-999_999_999_999_999.0), Some(-999_999_999_999_999)),
            (json!(-9_007_199_254_740_991.0), None),
            (json!(9_007_199_254_740_992.0), None),
            (json!(9_007_199_254_740_993.0_f64), None),
            (json!(1e300), None),
            (json!(f64::MAX), None),
            (json!(5.5), None),
            (json!(-0.25), None),
        ] {
            assert_eq!(exact_int(&number(value.clone())), expected, "{value}");
        }
    }

    tool_args! {
        #[derive(Debug, PartialEq)]
        struct Probe {
            count: u32 = Spec::int("How many", 0, 10),
            level: Option<u8> = Spec::int("How high", 0, 3),
            offset: Option<i64> = Spec::int("Where", 0, i64::MAX.unsigned_abs()),
            width: Option<usize> = Spec::int("How wide", 1, 64),
            ratio: Option<f32> = Spec::Num { desc: "A ratio", min: 0.0, max: 1.0 },
            label: Option<String> = Spec::bytes("A label", 1, 6),
            tags: Option<Vec<String>> = Spec::bytes_array("Tags", 2, 3),
            on: Option<bool> = Spec::flag("Whether"),
        }
    }

    fn probe(arguments: Value) -> Result<Probe, Vec<String>> {
        Probe::from_args(&arguments)
            .map_err(|violations| violations.iter().map(Violation::render).collect())
    }

    #[test]
    fn extraction_converts_whole_floats_and_never_falls_back() {
        let parsed = probe(json!({ "count": 5.0, "level": 1.0, "offset": 3.0, "width": 2e1 }))
            .expect("whole floats convert");
        assert_eq!(
            (parsed.count, parsed.level, parsed.offset, parsed.width),
            (5, Some(1), Some(3), Some(20))
        );
        for (arguments, message) in [
            (
                json!({ "count": 5.5 }),
                "count: expected integer, got number",
            ),
            (
                json!({ "count": "5" }),
                "count: expected integer, got string",
            ),
            (
                json!({ "count": 4_294_967_296_u64 }),
                "count: must fit in u32",
            ),
            (json!({ "count": -1 }), "count: must fit in u32"),
            (json!({ "count": 1, "level": 256 }), "level: must fit in u8"),
            (
                json!({ "count": 1, "offset": 9_223_372_036_854_775_808_u64 }),
                "offset: must fit in i64",
            ),
            (
                json!({ "count": 1, "offset": 9_223_372_036_854_775_808.0 }),
                "offset: expected integer, got float above 999999999999999; send an integer literal",
            ),
            (
                json!({ "count": 1, "ratio": "x" }),
                "ratio: expected number, got string",
            ),
            (
                json!({ "count": 1, "on": 1 }),
                "on: expected boolean, got integer",
            ),
        ] {
            assert_eq!(
                probe(arguments.clone()),
                Err(vec![message.to_string()]),
                "{arguments}"
            );
        }
    }

    #[test]
    fn extraction_reads_null_as_absence_and_reports_everything_at_once() {
        let parsed = probe(json!({ "count": 1, "level": null })).expect("null is absent");
        assert_eq!(parsed.level, None);
        assert_eq!(
            probe(json!({ "count": null })),
            Err(vec!["count: required argument is missing".to_string()])
        );
        let found = probe(json!({ "level": "x", "stray": 1, "ratio": true })).unwrap_err();
        assert_eq!(found.len(), 4, "{found:?}");
        assert!(found
            .iter()
            .any(|m| m.starts_with("stray: unknown argument")));
        assert_eq!(
            probe(json!([1])),
            Err(vec!["expected object, got array".to_string()])
        );
    }

    #[test]
    fn a_byte_bound_counts_bytes_not_characters() {
        // Two euro signs are two characters and six bytes.
        assert!(probe(json!({ "count": 1, "label": "\u{20ac}\u{20ac}" })).is_ok());
        assert_eq!(
            probe(json!({ "count": 1, "label": "\u{20ac}\u{20ac}a" })),
            Err(vec![
                "label: must be at most 6 bytes of UTF-8, got 7".to_string()
            ])
        );
        assert_eq!(
            probe(json!({ "count": 1, "tags": ["abc", "\u{20ac}a"] })),
            Err(vec![
                "tags[1]: must be at most 3 bytes of UTF-8, got 4".to_string()
            ])
        );
    }

    #[test]
    fn a_declaration_prints_the_schema_extraction_reads() {
        assert_eq!(Probe::required(), vec!["count"]);
        assert_eq!(
            Probe::FIELDS,
            ["count", "level", "offset", "width", "ratio", "label", "tags", "on"]
        );
        let schema = Probe::object_schema();
        assert_eq!(schema["additionalProperties"], false);
        let label = &schema["properties"]["label"];
        assert_eq!(label["maxLength"], 6);
        assert_eq!(label["minLength"], 1);
        assert_eq!(label["description"], "A label (at most 6 bytes, UTF-8)");
        assert_eq!(
            schema["properties"]["tags"]["description"],
            "Tags (each item at most 3 bytes, UTF-8)"
        );
        assert_eq!(schema["properties"]["count"]["maximum"], 10);
        assert_eq!(schema["properties"]["ratio"]["type"], "number");
        assert!(unsupported_keywords(&schema).is_empty());
    }

    // -- adversarial --

    /// A float spelled `N.0` or `NeX` inside the exact range is the integer it
    /// says. serde_json without `float_roundtrip` can parse the digits one ulp
    /// off, and `exact_int` then converts a different integer without noticing.
    #[test]
    fn a_whole_float_inside_the_exact_range_is_read_as_the_integer_it_spells() {
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut wrong = Vec::new();
        let mut cases: Vec<i128> = vec![
            999_999_999_999_999,
            999_999_999_999_998,
            450_359_962_737_049,
            100_000_000_000_001,
            123_456_789_012_345,
        ];
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            cases.push(i128::from(state % 999_999_999_999_999));
        }
        for n in cases {
            for text in [format!("{n}.0"), format!("-{n}.0")] {
                let want = if text.starts_with('-') { -n } else { n };
                if exact_int(&raw_number(&text)) != Some(want) {
                    wrong.push(text);
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "{} whole floats were read as another integer, e.g. {:?}",
            wrong.len(),
            &wrong[..wrong.len().min(5)]
        );
    }

    /// Above 10^15 - 1 a float spelling can parse one ulp off what the client
    /// wrote (serde_json has no `float_roundtrip`), so it is refused, with a
    /// message that names the argument and says what to send, never converted.
    #[test]
    fn a_whole_float_above_the_limit_is_refused_not_misread() {
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let schema = json!({
            "type": "object",
            "properties": { "revision": { "type": "integer" } }
        });
        for index in 0..20_000_u64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let n = 1_000_000_000_000_000 + state % 8_000_000_000_000_000;
            for text in [format!("{n}.0"), format!("-{n}.0")] {
                assert_eq!(exact_int(&raw_number(&text)), None, "{text}");
                let call = format!(r#"{{"revision": {text}}}"#);
                let found = validate(&serde_json::from_str::<Value>(&call).unwrap(), &schema);
                assert_eq!(found.len(), 1, "{text}");
                let message = found[0].render();
                assert!(
                    message.starts_with("revision:") && message.contains("integer literal"),
                    "{message}"
                );
            }
            if index == 0 {
                // The same value as an integer literal is read exactly.
                assert_eq!(exact_int(&raw_number(&format!("{n}"))), Some(i128::from(n)));
            }
        }
    }

    fn raw_number(text: &str) -> Number {
        match serde_json::from_str::<Value>(text).unwrap_or_else(|e| panic!("{text}: {e}")) {
            Value::Number(number) => number,
            other => panic!("{text} is {other}"),
        }
    }

    /// Raw JSON text, as a client writes it, against what `exact_int` makes of it.
    #[test]
    fn exact_int_reads_every_spelling_a_client_can_write() {
        let table: [(&str, Option<i128>); 33] = [
            ("0", Some(0)),
            ("-0", Some(0)),
            ("-0.0", Some(0)),
            ("0.0", Some(0)),
            ("5.0", Some(5)),
            ("5.5", None),
            ("1e2", Some(100)),
            ("1E2", Some(100)),
            ("1e+2", Some(100)),
            ("100e-2", Some(1)),
            ("1e-320", None),
            ("5e-324", None),
            ("1e300", None),
            ("-1e300", None),
            ("9007199254740990", Some(9_007_199_254_740_990)),
            ("9007199254740991", Some(9_007_199_254_740_991)),
            ("999999999999999.0", Some(999_999_999_999_999)),
            ("1000000000000000.0", None),
            ("9007199254740991.0", None),
            // A JavaScript client writes 2^53 as an integer literal.
            ("9007199254740992", Some(9_007_199_254_740_992)),
            ("9007199254740992.0", None),
            ("9007199254740993", Some(9_007_199_254_740_993)),
            ("9007199254740993.0", None),
            ("-999999999999999.0", Some(-999_999_999_999_999)),
            ("-9007199254740991.0", None),
            ("-9007199254740992.0", None),
            ("9223372036854775807", Some(9_223_372_036_854_775_807)),
            ("9223372036854775808", Some(9_223_372_036_854_775_808)),
            ("-9223372036854775808", Some(-9_223_372_036_854_775_808)),
            ("-9223372036854775809", None),
            ("18446744073709551615", Some(18_446_744_073_709_551_615)),
            ("18446744073709551616", None),
            ("1.0000000000000002", None),
        ];
        for (text, expected) in table {
            assert_eq!(exact_int(&raw_number(text)), expected, "{text}");
            let schema = json!({ "type": "integer" });
            assert_eq!(
                validate(&number_value(text), &schema).is_empty(),
                expected.is_some(),
                "the validator and exact_int disagree on {text}"
            );
        }
    }

    fn number_value(text: &str) -> Value {
        Value::Number(raw_number(text))
    }

    /// Two integers compare as integers: through `f64` the neighbours of 2^63
    /// and 2^64 are one number.
    #[test]
    fn bounds_compare_integer_literals_exactly_at_the_edge_of_i64_and_u64() {
        let signed = json!({ "type": "integer", "minimum": 0, "maximum": i64::MAX });
        assert!(validate(&number_value("9223372036854775807"), &signed).is_empty());
        assert_eq!(
            validate(&number_value("9223372036854775808"), &signed).len(),
            1
        );
        let unsigned = json!({ "type": "integer", "minimum": 0, "maximum": u64::MAX });
        assert!(validate(&number_value("18446744073709551615"), &unsigned).is_empty());
        assert_eq!(
            validate(&number_value("18446744073709551616"), &unsigned).len(),
            1,
            "2^64 passed a u64 maximum"
        );
        let lowest = json!({ "type": "integer", "minimum": i64::MIN });
        assert!(validate(&number_value("-9223372036854775808"), &lowest).is_empty());
        assert_eq!(
            validate(&number_value("-9223372036854775809"), &lowest).len(),
            1
        );
        let exact = json!({ "type": "integer", "minimum": 9_007_199_254_740_993_i64 });
        assert_eq!(validate(&number_value("9007199254740992"), &exact).len(), 1);
        assert!(validate(&number_value("9007199254740993"), &exact).is_empty());
    }

    /// A model that sends a thousand wrong things is told about them, but the
    /// answer must stay a record the transport can write: the stdio server
    /// drops a response over its output bound, and then the caller hears nothing.
    #[test]
    fn a_refusal_over_a_hundred_thousand_violations_is_still_a_small_message() {
        let closed = schema();
        let mut arguments = serde_json::Map::new();
        arguments.insert("repo_path".into(), json!("/tmp/x"));
        for index in 0..100_000 {
            arguments.insert(format!("junk{index}"), json!(1));
        }
        let violations = validate(&Value::Object(arguments), &closed);
        assert!(violations.len() >= 100_000);
        let message = refusal("probe", &violations);
        assert!(
            message.len() <= 64 * 1024,
            "a refusal of {} violations is {} bytes",
            violations.len(),
            message.len()
        );
        assert!(message.contains("junk0"), "it must still name an argument");
    }

    #[test]
    fn a_refusal_never_echoes_a_hostile_value() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "enum": ["fast", "full"], "maxLength": 32 },
                "n": { "type": "integer", "minimum": 1, "maximum": 5 }
            },
            "additionalProperties": false
        });
        let long = "Z".repeat(1 << 20);
        let violations = validate(&json!({ "mode": long, "n": [long] }), &schema);
        let message = refusal("probe", &violations);
        assert!(message.len() < 4096, "{} bytes", message.len());
        assert!(!message.contains("ZZZZ"));
    }
}
