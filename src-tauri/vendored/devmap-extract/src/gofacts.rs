//! Go declarations and expressions, in a form a resolver can evaluate.
//!
//! The call ladder types a receiver from *names*: a parameter's bare type
//! name, a constructor's bare return type, a field's bare type name. That is
//! enough for `s := NewStore(); s.Get()` and for nothing that Go's type flow
//! goes through next — a multi-value `:=`, a method's result, a chain, a range
//! variable, an element, a type switch, a qualified field type, an embedded
//! field, an interface. Each of those was a separate "known gap" because the
//! facts that answer them were never extracted: the extractor reduced every
//! type to one identifier and every receiver to its leftmost segment.
//!
//! This module extracts the facts whole and leaves the *evaluation* to
//! `devmap-resolve`, which holds every file at once and can follow a type
//! across packages. It records, per file:
//!
//! * package-level type declarations (struct fields with embedding, interface
//!   method sets and embedded interfaces, aliases, defined types), function and
//!   method signatures, and package-level variables;
//! * the **scope** of every local the file binds — a parameter, a `:=`, a
//!   `var`, a range variable, a type-switch clause — as a byte range plus how
//!   its type is known, so a use at byte `p` finds the binding the language
//!   would, shadowing included, without the extractor having to guess which
//!   assignment a name refers to;
//! * for every method-style call `recv.name(...)`, the receiver as a structured
//!   expression rather than the reduced identity `ExtractedCall::receiver_expr`
//!   keeps for the ledger.
//!
//! Everything is syntactic. Nothing here resolves a name; whether `store` is a
//! package, a local or a type is the resolver's question.
//!
//! Bounded throughout: expressions and types stop at [`MAX_DEPTH`] and become
//! `Unknown`, and a file records at most [`MAX_LOCALS`] scoped bindings, so
//! hostile nesting or a generated file cannot inflate the payload.

use serde::{Deserialize, Serialize};

/// A Go type expression, as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoType {
    /// `T`, `pkg.T`, `T[A, B]`. `pkg` is the qualifier as written; whether it
    /// names an indexed package or an external one is the resolver's call.
    Name {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pkg: Option<String>,
        name: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<GoType>,
    },
    Ptr(Box<GoType>),
    Slice(Box<GoType>),
    Array(Box<GoType>),
    Map(Box<GoType>, Box<GoType>),
    Chan(Box<GoType>),
    Func {
        params: Vec<GoType>,
        results: Vec<GoType>,
    },
    /// An anonymous `struct{…}` or `interface{…}` literal: opaque.
    Opaque,
    Unknown,
}

/// A Go expression, reduced to what typing it needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoExpr {
    Ident(String),
    /// `x.name`: a field, a method, or a package member.
    Selector(Box<GoExpr>, String),
    /// `f(...)`, `f[T](...)`. `arg0` is the first argument for the builtins
    /// whose result type is theirs to say (`new`, `make`, `append`).
    Call {
        fun: Box<GoExpr>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        targs: Vec<GoType>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arg0: Option<Box<GoExpr>>,
    },
    /// `x[i]`, or the instantiation `f[T]` / `G[T]` — the resolver decides by
    /// what `x` is. `targs` holds the index read as types when it can be.
    Index(Box<GoExpr>, Vec<GoType>),
    /// `x[a:b]`.
    Slice(Box<GoExpr>),
    /// `*x`: a dereference, or the pointer type `*T` in `(*T).M`.
    Star(Box<GoExpr>),
    /// `&x`.
    Addr(Box<GoExpr>),
    /// `<-ch`.
    Recv(Box<GoExpr>),
    /// `T{…}`, `[]T{…}`.
    Composite(GoType),
    /// `x.(T)`.
    Assert(GoType),
    /// An expression position that is syntactically a type (`map[K]V`).
    Type(GoType),
    /// `func(...) ... { … }`.
    FuncLit {
        params: Vec<GoType>,
        results: Vec<GoType>,
    },
    Unknown,
}

/// One struct field. `embedded` fields are named by their type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoField {
    pub name: String,
    pub ty: GoType,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub embedded: bool,
}

/// One interface method requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoIfaceMethod {
    pub name: String,
    pub params: Vec<GoType>,
    pub results: Vec<GoType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoTypeBody {
    Struct(Vec<GoField>),
    Interface {
        methods: Vec<GoIfaceMethod>,
        embeds: Vec<GoType>,
    },
    /// `type A = T`.
    Alias(GoType),
    /// `type D T` — a new type over `T`'s underlying type, with none of its
    /// methods.
    Defined(GoType),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoTypeDecl {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_params: Vec<String>,
    pub body: GoTypeBody,
}

/// A receiver, `(g *G[T, K])`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoRecv {
    /// The receiver's base type name (`G`), type arguments and pointer removed.
    pub type_name: String,
    pub ptr: bool,
    /// The names the receiver gives the type's parameters (`T`, `K`), in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoFunc {
    /// The symbol's `qualified_name`.
    pub qualified_name: String,
    /// Byte span of the declaration: what a use position is placed in to find
    /// the type parameters in scope.
    pub start: usize,
    pub end: usize,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recv: Option<GoRecv>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_params: Vec<String>,
    pub params: Vec<GoType>,
    pub results: Vec<GoType>,
}

/// How a binding's type is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoBinding {
    /// Written: a parameter, `var x T`, a one-type type-switch clause.
    Type(GoType),
    /// The type of an expression: `x := e`, `var x = e`.
    Expr(GoExpr),
    /// One position of a tuple: `a, b := f()`, `v, ok := m[k]`.
    Tuple { source: GoExpr, index: usize },
    /// A range variable: the key (first) or the value (second).
    Range { over: GoExpr, key: bool },
}

/// A local binding and the byte range in which it is in scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoLocal {
    pub name: String,
    pub from: usize,
    pub to: usize,
    pub binding: GoBinding,
}

/// A package-level variable or constant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoVar {
    pub name: String,
    pub binding: GoBinding,
}

/// A function-local type declaration's name and scope: while it is in scope it
/// shadows a package-level type of the same name, and nothing here describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoLocalType {
    pub name: String,
    pub from: usize,
    pub to: usize,
}

/// A method-style call `recv.name(...)` and its receiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoCallSite {
    /// Byte span of the call expression: the key `ExtractedCall::span` shares.
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub recv: GoExpr,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoFacts {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<GoTypeDecl>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub funcs: Vec<GoFunc>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vars: Vec<GoVar>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locals: Vec<GoLocal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_types: Vec<GoLocalType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub call_sites: Vec<GoCallSite>,
    /// Set when a cap above was hit: the facts are a prefix, and the resolver
    /// must not read a missing binding as "none".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

#[cfg(feature = "parse")]
mod build {
    use super::*;
    use tree_sitter::Node;

    /// Deepest expression or type nesting recorded; deeper becomes `Unknown`.
    const MAX_DEPTH: usize = 24;
    /// Most scoped local bindings one file records.
    const MAX_LOCALS: usize = 20_000;
    /// Most call sites with a structured receiver one file records.
    const MAX_CALL_SITES: usize = 50_000;
    /// Deepest AST recursion the visitor takes.
    const MAX_AST_DEPTH: usize = 256;

    fn txt<'a>(node: Node, source: &'a str) -> &'a str {
        source.get(node.start_byte()..node.end_byte()).unwrap_or("")
    }

    /// Extract the Go facts of a parsed file. `file` is the file symbol's name, the
    /// prefix of every `qualified_name`.
    pub fn extract(root: Node, source: &str, file: &str) -> GoFacts {
        let mut builder = Builder {
            source,
            file,
            facts: GoFacts::default(),
        };
        builder.visit(root, 0, false);
        builder.facts
    }

    struct Builder<'s> {
        source: &'s str,
        file: &'s str,
        facts: GoFacts,
    }

    impl<'s> Builder<'s> {
        fn text(&self, node: Node) -> &'s str {
            txt(node, self.source)
        }

        fn visit(&mut self, node: Node, depth: usize, in_func: bool) {
            if depth > MAX_AST_DEPTH {
                self.facts.truncated = true;
                return;
            }
            let mut in_func = in_func;
            match node.kind() {
                "function_declaration" | "method_declaration" => {
                    self.function(node);
                    in_func = true;
                }
                "func_literal" => {
                    self.params_scope(node, node.start_byte(), node.end_byte());
                    in_func = true;
                }
                "type_declaration" if !in_func => {
                    let mut cursor = node.walk();
                    for spec in node.named_children(&mut cursor) {
                        if matches!(spec.kind(), "type_spec" | "type_alias") {
                            self.type_decl(spec);
                        }
                    }
                }
                "type_declaration" => {
                    let mut cursor = node.walk();
                    for spec in node.named_children(&mut cursor) {
                        if matches!(spec.kind(), "type_spec" | "type_alias") {
                            if let Some(name) = spec.child_by_field_name("name") {
                                self.facts.local_types.push(GoLocalType {
                                    name: self.text(name).to_string(),
                                    from: spec.start_byte(),
                                    to: self.scope_end(spec),
                                });
                            }
                        }
                    }
                }
                "var_declaration" | "const_declaration" if !in_func => {
                    let mut cursor = node.walk();
                    for spec in node.named_children(&mut cursor) {
                        if matches!(spec.kind(), "var_spec" | "const_spec") {
                            self.package_var(spec);
                        }
                    }
                }
                "var_spec" | "const_spec" if in_func => self.local_var_spec(node),
                "short_var_declaration" => self.short_var(node),
                "range_clause" => self.range_clause(node),
                "type_switch_statement" => self.type_switch(node),
                "receive_statement" => self.receive(node),
                "call_expression" => self.call_site(node),
                _ => {}
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                self.visit(child, depth + 1, in_func);
            }
        }

        // -- types ---------------------------------------------------------------

        fn ty(&self, node: Node, depth: usize) -> GoType {
            if depth > MAX_DEPTH {
                return GoType::Unknown;
            }
            match node.kind() {
                "type_identifier" | "identifier" => GoType::Name {
                    pkg: None,
                    name: self.text(node).to_string(),
                    args: Vec::new(),
                },
                "qualified_type" => {
                    let name = node.child_by_field_name("name").map(|n| self.text(n));
                    let pkg = node.child_by_field_name("package").map(|n| self.text(n));
                    match (name, pkg) {
                        (Some(name), Some(pkg)) => GoType::Name {
                            pkg: Some(pkg.to_string()),
                            name: name.to_string(),
                            args: Vec::new(),
                        },
                        _ => GoType::Unknown,
                    }
                }
                "generic_type" => {
                    let Some(inner) = node.child_by_field_name("type") else {
                        return GoType::Unknown;
                    };
                    let mut base = self.ty(inner, depth + 1);
                    let mut collected = Vec::new();
                    if let Some(arguments) = node.child_by_field_name("type_arguments") {
                        let mut cursor = arguments.walk();
                        for argument in arguments.named_children(&mut cursor) {
                            collected.push(self.ty(argument, depth + 1));
                        }
                    }
                    if let GoType::Name { args, .. } = &mut base {
                        *args = collected;
                    }
                    base
                }
                "type_elem" | "parenthesized_type" | "type_constraint" => node
                    .named_child(0)
                    .map(|inner| self.ty(inner, depth + 1))
                    .unwrap_or(GoType::Unknown),
                "pointer_type" => node
                    .named_child(0)
                    .map(|inner| GoType::Ptr(Box::new(self.ty(inner, depth + 1))))
                    .unwrap_or(GoType::Unknown),
                "slice_type" => node
                    .child_by_field_name("element")
                    .map(|inner| GoType::Slice(Box::new(self.ty(inner, depth + 1))))
                    .unwrap_or(GoType::Unknown),
                "array_type" | "implicit_length_array_type" => node
                    .child_by_field_name("element")
                    .map(|inner| GoType::Array(Box::new(self.ty(inner, depth + 1))))
                    .unwrap_or(GoType::Unknown),
                "map_type" => match (
                    node.child_by_field_name("key"),
                    node.child_by_field_name("value"),
                ) {
                    (Some(key), Some(value)) => GoType::Map(
                        Box::new(self.ty(key, depth + 1)),
                        Box::new(self.ty(value, depth + 1)),
                    ),
                    _ => GoType::Unknown,
                },
                "channel_type" => node
                    .child_by_field_name("value")
                    .map(|inner| GoType::Chan(Box::new(self.ty(inner, depth + 1))))
                    .unwrap_or(GoType::Unknown),
                "function_type" => GoType::Func {
                    params: node
                        .child_by_field_name("parameters")
                        .map(|list| self.param_types(list, depth + 1))
                        .unwrap_or_default(),
                    results: self.result_types(node, depth + 1),
                },
                "struct_type" | "interface_type" => GoType::Opaque,
                _ => GoType::Unknown,
            }
        }

        /// The type of each parameter of a `parameter_list`, one entry per name
        /// (`a, b int` is two), a variadic parameter as a slice.
        fn param_types(&self, list: Node, depth: usize) -> Vec<GoType> {
            self.params(list, depth)
                .into_iter()
                .map(|(_, ty)| ty)
                .collect()
        }

        fn params(&self, list: Node, depth: usize) -> Vec<(Option<String>, GoType)> {
            let mut out = Vec::new();
            let mut cursor = list.walk();
            for decl in list.named_children(&mut cursor) {
                let variadic = match decl.kind() {
                    "parameter_declaration" => false,
                    "variadic_parameter_declaration" => true,
                    _ => continue,
                };
                let Some(ty_node) = decl.child_by_field_name("type") else {
                    continue;
                };
                let mut ty = self.ty(ty_node, depth + 1);
                if variadic {
                    ty = GoType::Slice(Box::new(ty));
                }
                let mut names = decl.walk();
                let named: Vec<String> = decl
                    .children_by_field_name("name", &mut names)
                    .map(|name| self.text(name).to_string())
                    .collect();
                if named.is_empty() {
                    out.push((None, ty));
                } else {
                    for name in named {
                        out.push((Some(name), ty.clone()));
                    }
                }
            }
            out
        }

        fn result_types(&self, callable: Node, depth: usize) -> Vec<GoType> {
            match callable.child_by_field_name("result") {
                None => Vec::new(),
                Some(result) if result.kind() == "parameter_list" => {
                    self.param_types(result, depth)
                }
                Some(result) => vec![self.ty(result, depth)],
            }
        }

        fn type_param_names(&self, list: Option<Node>) -> Vec<String> {
            let Some(list) = list else {
                return Vec::new();
            };
            let mut out = Vec::new();
            let mut cursor = list.walk();
            for decl in list.named_children(&mut cursor) {
                let mut names = decl.walk();
                for name in decl.children_by_field_name("name", &mut names) {
                    out.push(self.text(name).to_string());
                }
            }
            out
        }

        // -- declarations --------------------------------------------------------

        fn type_decl(&mut self, spec: Node) {
            let Some(name) = spec.child_by_field_name("name") else {
                return;
            };
            let Some(ty_node) = spec.child_by_field_name("type") else {
                return;
            };
            let type_params = self.type_param_names(spec.child_by_field_name("type_parameters"));
            let body = match ty_node.kind() {
                "struct_type" => GoTypeBody::Struct(self.struct_fields(ty_node)),
                "interface_type" => self.interface_body(ty_node),
                _ if spec.kind() == "type_alias" => GoTypeBody::Alias(self.ty(ty_node, 0)),
                _ => GoTypeBody::Defined(self.ty(ty_node, 0)),
            };
            self.facts.types.push(GoTypeDecl {
                name: self.text(name).to_string(),
                type_params,
                body,
            });
        }

        fn struct_fields(&self, struct_type: Node) -> Vec<GoField> {
            let mut out = Vec::new();
            let mut cursor = struct_type.walk();
            let Some(list) = struct_type
                .named_children(&mut cursor)
                .find(|child| child.kind() == "field_declaration_list")
            else {
                return out;
            };
            let mut cursor = list.walk();
            for decl in list.named_children(&mut cursor) {
                if decl.kind() != "field_declaration" {
                    continue;
                }
                let Some(ty_node) = decl.child_by_field_name("type") else {
                    continue;
                };
                let ty = self.ty(ty_node, 0);
                let mut names = decl.walk();
                let named: Vec<String> = decl
                    .children_by_field_name("name", &mut names)
                    .map(|name| self.text(name).to_string())
                    .collect();
                if named.is_empty() {
                    // Embedded: the field is named by its type, pointer and
                    // qualifier stripped.
                    let mut base = &ty;
                    while let GoType::Ptr(inner) = base {
                        base = inner;
                    }
                    if let GoType::Name { name, .. } = base {
                        out.push(GoField {
                            name: name.clone(),
                            ty,
                            embedded: true,
                        });
                    }
                } else {
                    for name in named {
                        out.push(GoField {
                            name,
                            ty: ty.clone(),
                            embedded: false,
                        });
                    }
                }
            }
            out
        }

        fn interface_body(&self, interface_type: Node) -> GoTypeBody {
            let mut methods = Vec::new();
            let mut embeds = Vec::new();
            let mut cursor = interface_type.walk();
            for child in interface_type.named_children(&mut cursor) {
                match child.kind() {
                    "method_elem" => {
                        let Some(name) = child.child_by_field_name("name") else {
                            continue;
                        };
                        methods.push(GoIfaceMethod {
                            name: self.text(name).to_string(),
                            params: child
                                .child_by_field_name("parameters")
                                .map(|list| self.param_types(list, 0))
                                .unwrap_or_default(),
                            results: self.result_types(child, 0),
                        });
                    }
                    "type_elem" if child.named_child_count() == 1 => {
                        // One named type is an embedded interface; a union or a
                        // `~T` term is a type-set constraint and adds no method.
                        if let Some(only) = child.named_child(0).filter(|only| {
                            matches!(
                                only.kind(),
                                "type_identifier" | "qualified_type" | "generic_type"
                            )
                        }) {
                            embeds.push(self.ty(only, 0));
                        }
                    }
                    _ => {}
                }
            }
            GoTypeBody::Interface { methods, embeds }
        }

        fn function(&mut self, node: Node) {
            let Some(name) = node.child_by_field_name("name") else {
                return;
            };
            let name = self.text(name).to_string();
            let recv = (node.kind() == "method_declaration")
                .then(|| self.receiver(node))
                .flatten();
            let qualified_name = match &recv {
                Some((_, recv)) => format!("{}::{}.{}", self.file, recv.type_name, name),
                None => format!("{}::{}", self.file, name),
            };
            let params = node
                .child_by_field_name("parameters")
                .map(|list| self.param_types(list, 0))
                .unwrap_or_default();
            let results = self.result_types(node, 0);
            self.params_scope(node, node.start_byte(), node.end_byte());
            self.facts.funcs.push(GoFunc {
                qualified_name,
                start: node.start_byte(),
                end: node.end_byte(),
                name,
                recv: recv.map(|(_, recv)| recv),
                type_params: self.type_param_names(node.child_by_field_name("type_parameters")),
                params,
                results,
            });
        }

        /// The receiver's binding name (empty when unnamed) and base type.
        fn receiver(&self, method: Node) -> Option<(String, GoRecv)> {
            let list = method.child_by_field_name("receiver")?;
            let mut cursor = list.walk();
            let decl = list
                .named_children(&mut cursor)
                .find(|child| child.kind() == "parameter_declaration")?;
            let ty_node = decl.child_by_field_name("type")?;
            let name = decl
                .child_by_field_name("name")
                .map(|name| self.text(name).to_string())
                .unwrap_or_default();
            let ty = self.ty(ty_node, 0);
            let (ptr, base) = match &ty {
                GoType::Ptr(inner) => (true, inner.as_ref()),
                other => (false, other),
            };
            let GoType::Name {
                name: type_name,
                args,
                ..
            } = base
            else {
                return None;
            };
            let type_params = args
                .iter()
                .filter_map(|arg| match arg {
                    GoType::Name {
                        pkg: None, name, ..
                    } => Some(name.clone()),
                    _ => None,
                })
                .collect();
            Some((
                name,
                GoRecv {
                    type_name: type_name.clone(),
                    ptr,
                    type_params,
                },
            ))
        }

        /// Record the parameters, receiver and named results of `callable` as
        /// locals scoped to `[from, to)`.
        fn params_scope(&mut self, callable: Node, from: usize, to: usize) {
            if let Some(list) = callable.child_by_field_name("receiver") {
                let mut cursor = list.walk();
                for decl in list.named_children(&mut cursor) {
                    if decl.kind() != "parameter_declaration" {
                        continue;
                    }
                    let (Some(name), Some(ty)) = (
                        decl.child_by_field_name("name"),
                        decl.child_by_field_name("type"),
                    ) else {
                        continue;
                    };
                    let binding = GoBinding::Type(self.ty(ty, 0));
                    self.push_local(self.text(name).to_string(), from, to, binding);
                }
            }
            let mut lists = Vec::new();
            if let Some(list) = callable.child_by_field_name("parameters") {
                lists.push(list);
            }
            if let Some(result) = callable.child_by_field_name("result") {
                if result.kind() == "parameter_list" {
                    lists.push(result);
                }
            }
            for list in lists {
                for (name, ty) in self.params(list, 0) {
                    if let Some(name) = name {
                        self.push_local(name, from, to, GoBinding::Type(ty));
                    }
                }
            }
        }

        fn package_var(&mut self, spec: Node) {
            for (name, binding) in self.spec_bindings(spec) {
                self.facts.vars.push(GoVar { name, binding });
            }
        }

        fn local_var_spec(&mut self, spec: Node) {
            let from = spec.end_byte();
            let to = self.scope_end(spec);
            for (name, binding) in self.spec_bindings(spec) {
                self.push_local(name, from, to, binding);
            }
        }

        /// `var a, b T`, `var a, b = f()`, `const c = e`: each name and how it is typed.
        fn spec_bindings(&self, spec: Node) -> Vec<(String, GoBinding)> {
            let mut cursor = spec.walk();
            let names: Vec<String> = spec
                .children_by_field_name("name", &mut cursor)
                .filter(|name| name.kind() == "identifier")
                .map(|name| self.text(name).to_string())
                .collect();
            if let Some(ty) = spec.child_by_field_name("type") {
                let ty = self.ty(ty, 0);
                return names
                    .into_iter()
                    .map(|name| (name, GoBinding::Type(ty.clone())))
                    .collect();
            }
            let values = spec
                .child_by_field_name("value")
                .map(|list| self.expr_list(list))
                .unwrap_or_default();
            self.assign_bindings(names, values)
        }

        fn expr_list(&self, list: Node) -> Vec<GoExpr> {
            if list.kind() != "expression_list" {
                return vec![self.expr(list, 0)];
            }
            let mut cursor = list.walk();
            list.named_children(&mut cursor)
                .map(|item| self.expr(item, 0))
                .collect()
        }

        /// Pair names with values: position by position when the counts match, a
        /// tuple destructure when one value feeds several names.
        fn assign_bindings(
            &self,
            names: Vec<String>,
            values: Vec<GoExpr>,
        ) -> Vec<(String, GoBinding)> {
            if names.len() == values.len() {
                return names
                    .into_iter()
                    .zip(values)
                    .map(|(name, value)| (name, GoBinding::Expr(value)))
                    .collect();
            }
            if values.len() == 1 {
                let source = values.into_iter().next().unwrap_or(GoExpr::Unknown);
                return names
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| {
                        (
                            name,
                            GoBinding::Tuple {
                                source: source.clone(),
                                index,
                            },
                        )
                    })
                    .collect();
            }
            Vec::new()
        }

        fn short_var(&mut self, node: Node) {
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                return;
            };
            let names = self.identifier_names(left);
            let bindings = self.assign_bindings(names, self.expr_list(right));
            let from = node.end_byte();
            let to = self.scope_end(node);
            for (name, binding) in bindings {
                self.push_local(name, from, to, binding);
            }
        }

        /// Names of an `expression_list` of plain identifiers; a position that is
        /// not one (or is `_`) keeps its slot as an empty name so tuple indexes line up.
        fn identifier_names(&self, list: Node) -> Vec<String> {
            let mut cursor = list.walk();
            let items: Vec<Node> = if list.kind() == "expression_list" {
                list.named_children(&mut cursor).collect()
            } else {
                vec![list]
            };
            items
                .into_iter()
                .map(|item| {
                    if item.kind() == "identifier" && self.text(item) != "_" {
                        self.text(item).to_string()
                    } else {
                        String::new()
                    }
                })
                .collect()
        }

        fn range_clause(&mut self, node: Node) {
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                return;
            };
            // `for i, v = range xs` assigns existing variables; only `:=` declares.
            let declares = {
                let mut cursor = node.walk();
                let declares = node.children(&mut cursor).any(|child| child.kind() == ":=");
                declares
            };
            if !declares {
                return;
            }
            let over = self.expr(right, 0);
            let from = node.end_byte();
            let to = node
                .parent()
                .map(|parent| parent.end_byte())
                .unwrap_or(from);
            for (position, name) in self.identifier_names(left).into_iter().enumerate().take(2) {
                if name.is_empty() {
                    continue;
                }
                self.push_local(
                    name,
                    from,
                    to,
                    GoBinding::Range {
                        over: over.clone(),
                        key: position == 0,
                    },
                );
            }
        }

        fn type_switch(&mut self, node: Node) {
            let (Some(alias), Some(value)) = (
                node.child_by_field_name("alias"),
                node.child_by_field_name("value"),
            ) else {
                return;
            };
            let Some(name) = self
                .identifier_names(alias)
                .into_iter()
                .next()
                .filter(|name| !name.is_empty())
            else {
                return;
            };
            let subject = self.expr(value, 0);
            let mut cursor = node.walk();
            for clause in node.named_children(&mut cursor) {
                if !matches!(clause.kind(), "type_case" | "default_case") {
                    continue;
                }
                let mut types_cursor = clause.walk();
                let types: Vec<Node> = clause
                    .children_by_field_name("type", &mut types_cursor)
                    .filter(|ty| ty.is_named())
                    .collect();
                // One type fixes the variable's type; several, or `default`, leave
                // it the subject's.
                let binding = match types.as_slice() {
                    [only] => GoBinding::Type(self.ty(*only, 0)),
                    _ => GoBinding::Expr(subject.clone()),
                };
                self.push_local(
                    name.clone(),
                    clause.start_byte(),
                    clause.end_byte(),
                    binding,
                );
            }
        }

        fn receive(&mut self, node: Node) {
            let (Some(left), Some(right)) = (
                node.child_by_field_name("left"),
                node.child_by_field_name("right"),
            ) else {
                return;
            };
            let names = self.identifier_names(left);
            let bindings = self.assign_bindings(names, vec![self.expr(right, 0)]);
            let to = self.scope_end(node);
            for (name, binding) in bindings {
                self.push_local(name, node.end_byte(), to, binding);
            }
        }

        fn call_site(&mut self, node: Node) {
            let Some(function) = node.child_by_field_name("function") else {
                return;
            };
            let mut function = function;
            while function.kind() == "parenthesized_expression" {
                match function.named_child(0) {
                    Some(inner) => function = inner,
                    None => return,
                }
            }
            if function.kind() != "selector_expression" {
                return;
            }
            let (Some(operand), Some(field)) = (
                function.child_by_field_name("operand"),
                function.child_by_field_name("field"),
            ) else {
                return;
            };
            if self.facts.call_sites.len() >= MAX_CALL_SITES {
                self.facts.truncated = true;
                return;
            }
            let recv = self.expr(operand, 0);
            self.facts.call_sites.push(GoCallSite {
                start: node.start_byte(),
                end: node.end_byte(),
                name: self.text(field).to_string(),
                recv,
            });
        }

        fn push_local(&mut self, name: String, from: usize, to: usize, binding: GoBinding) {
            if name.is_empty() || name == "_" {
                return;
            }
            if self.facts.locals.len() >= MAX_LOCALS {
                self.facts.truncated = true;
                return;
            }
            self.facts.locals.push(GoLocal {
                name,
                from,
                to,
                binding,
            });
        }

        /// The end of the innermost scope a declaration at `node` lives in: its
        /// block, its clause, or the `if`/`for`/`switch` whose header declared it.
        fn scope_end(&self, node: Node) -> usize {
            let mut current = node;
            while let Some(parent) = current.parent() {
                match parent.kind() {
                    "block"
                    | "type_case"
                    | "expression_case"
                    | "default_case"
                    | "communication_case"
                    | "if_statement"
                    | "for_statement"
                    | "expression_switch_statement"
                    | "type_switch_statement"
                    | "select_statement"
                    | "func_literal"
                    | "function_declaration"
                    | "method_declaration"
                    | "source_file" => return parent.end_byte(),
                    _ => current = parent,
                }
            }
            node.end_byte()
        }

        // -- expressions ---------------------------------------------------------

        fn expr(&self, node: Node, depth: usize) -> GoExpr {
            if depth > MAX_DEPTH {
                return GoExpr::Unknown;
            }
            match node.kind() {
                "identifier" | "type_identifier" | "package_identifier" | "field_identifier" => {
                    GoExpr::Ident(self.text(node).to_string())
                }
                "parenthesized_expression" => node
                    .named_child(0)
                    .map(|inner| self.expr(inner, depth + 1))
                    .unwrap_or(GoExpr::Unknown),
                "selector_expression" => match (
                    node.child_by_field_name("operand"),
                    node.child_by_field_name("field"),
                ) {
                    (Some(operand), Some(field)) => GoExpr::Selector(
                        Box::new(self.expr(operand, depth + 1)),
                        self.text(field).to_string(),
                    ),
                    _ => GoExpr::Unknown,
                },
                "qualified_type" => match (
                    node.child_by_field_name("package"),
                    node.child_by_field_name("name"),
                ) {
                    (Some(pkg), Some(name)) => GoExpr::Selector(
                        Box::new(GoExpr::Ident(self.text(pkg).to_string())),
                        self.text(name).to_string(),
                    ),
                    _ => GoExpr::Unknown,
                },
                "call_expression" => self.call_expr(node, depth),
                "type_conversion_expression" => {
                    // `pkg.F[int](x)`: the grammar's reading of a one-type-argument
                    // call. The callee is the generic "type", spelled as the
                    // expression it is.
                    let Some(ty) = node.child_by_field_name("type") else {
                        return GoExpr::Unknown;
                    };
                    GoExpr::Call {
                        fun: Box::new(self.type_as_expr(ty, depth + 1)),
                        targs: Vec::new(),
                        arg0: None,
                    }
                }
                "index_expression" => {
                    let Some(operand) = node.child_by_field_name("operand") else {
                        return GoExpr::Unknown;
                    };
                    let targs = node
                        .child_by_field_name("index")
                        .map(|index| self.index_as_types(index, depth + 1))
                        .unwrap_or_default();
                    GoExpr::Index(Box::new(self.expr(operand, depth + 1)), targs)
                }
                "slice_expression" => node
                    .child_by_field_name("operand")
                    .map(|operand| GoExpr::Slice(Box::new(self.expr(operand, depth + 1))))
                    .unwrap_or(GoExpr::Unknown),
                "unary_expression" => {
                    let (Some(operator), Some(operand)) = (
                        node.child_by_field_name("operator"),
                        node.child_by_field_name("operand"),
                    ) else {
                        return GoExpr::Unknown;
                    };
                    let inner = Box::new(self.expr(operand, depth + 1));
                    match self.text(operator) {
                        "&" => GoExpr::Addr(inner),
                        "*" => GoExpr::Star(inner),
                        "<-" => GoExpr::Recv(inner),
                        _ => GoExpr::Unknown,
                    }
                }
                "composite_literal" => node
                    .child_by_field_name("type")
                    .map(|ty| GoExpr::Composite(self.ty(ty, depth + 1)))
                    .unwrap_or(GoExpr::Unknown),
                "type_assertion_expression" => node
                    .child_by_field_name("type")
                    .map(|ty| GoExpr::Assert(self.ty(ty, depth + 1)))
                    .unwrap_or(GoExpr::Unknown),
                "func_literal" => GoExpr::FuncLit {
                    params: node
                        .child_by_field_name("parameters")
                        .map(|list| self.param_types(list, depth + 1))
                        .unwrap_or_default(),
                    results: self.result_types(node, depth + 1),
                },
                "slice_type"
                | "array_type"
                | "implicit_length_array_type"
                | "map_type"
                | "channel_type"
                | "function_type"
                | "pointer_type"
                | "generic_type"
                | "struct_type"
                | "interface_type" => GoExpr::Type(self.ty(node, depth + 1)),
                _ => GoExpr::Unknown,
            }
        }

        fn call_expr(&self, node: Node, depth: usize) -> GoExpr {
            let Some(function) = node.child_by_field_name("function") else {
                return GoExpr::Unknown;
            };
            let mut targs = Vec::new();
            if let Some(arguments) = node.child_by_field_name("type_arguments") {
                let mut cursor = arguments.walk();
                for argument in arguments.named_children(&mut cursor) {
                    targs.push(self.ty(argument, depth + 1));
                }
            }
            // `new(T)`, `make(T, n)` and `append(s, …)` are typed by their first
            // argument; every other call by its callee.
            let arg0 = (function.kind() == "identifier"
                && matches!(self.text(function), "new" | "make" | "append"))
            .then(|| {
                node.child_by_field_name("arguments")
                    .and_then(|arguments| arguments.named_child(0))
                    .map(|first| Box::new(self.expr(first, depth + 1)))
            })
            .flatten();
            GoExpr::Call {
                fun: Box::new(self.expr(function, depth + 1)),
                targs,
                arg0,
            }
        }

        /// A type node spelled as the expression it denotes in a call position:
        /// `pkg.F[int]` is `Index(Selector(pkg, F), [int])`.
        fn type_as_expr(&self, node: Node, depth: usize) -> GoExpr {
            if depth > MAX_DEPTH {
                return GoExpr::Unknown;
            }
            match node.kind() {
                "type_identifier" => GoExpr::Ident(self.text(node).to_string()),
                "qualified_type" => self.expr(node, depth),
                "generic_type" => {
                    let Some(inner) = node.child_by_field_name("type") else {
                        return GoExpr::Unknown;
                    };
                    let mut targs = Vec::new();
                    if let Some(arguments) = node.child_by_field_name("type_arguments") {
                        let mut cursor = arguments.walk();
                        for argument in arguments.named_children(&mut cursor) {
                            targs.push(self.ty(argument, depth + 1));
                        }
                    }
                    GoExpr::Index(Box::new(self.type_as_expr(inner, depth + 1)), targs)
                }
                _ => GoExpr::Type(self.ty(node, depth)),
            }
        }

        /// An index expression read as type arguments: `F[int]`, `G[pkg.T]`,
        /// `F[int, string]`. Empty when it does not read as types (`xs[i+1]`).
        fn index_as_types(&self, index: Node, depth: usize) -> Vec<GoType> {
            match index.kind() {
                "identifier" | "type_identifier" => vec![self.ty(index, depth)],
                "selector_expression" => match (
                    index.child_by_field_name("operand"),
                    index.child_by_field_name("field"),
                ) {
                    (Some(operand), Some(field)) if operand.kind() == "identifier" => {
                        vec![GoType::Name {
                            pkg: Some(self.text(operand).to_string()),
                            name: self.text(field).to_string(),
                            args: Vec::new(),
                        }]
                    }
                    _ => Vec::new(),
                },
                "qualified_type" | "generic_type" | "pointer_type" | "slice_type" | "map_type"
                | "array_type" | "channel_type" | "type_elem" => vec![self.ty(index, depth)],
                "expression_list" => {
                    let mut cursor = index.walk();
                    let all: Vec<Vec<GoType>> = index
                        .named_children(&mut cursor)
                        .map(|item| self.index_as_types(item, depth + 1))
                        .collect();
                    if all.iter().any(Vec::is_empty) {
                        Vec::new()
                    } else {
                        all.into_iter().flatten().collect()
                    }
                }
                _ => Vec::new(),
            }
        }
    }
}

#[cfg(feature = "parse")]
pub use build::extract;
