//! Go type evaluation: what a receiver expression's type is, and which method
//! a call on it names.
//!
//! The call ladder's receiver rungs type a value from a *name* — a parameter's
//! bare type name, a constructor's bare return type — and dispatch on
//! `(family, bare type name, method)`. A bare name is not a type: it cannot say
//! which package declared `Buffer`, so `w.buf.Reset()` with `buf *bytes.Buffer`
//! bound to an in-repo `Buffer.Reset` at DETERMINISTIC; and it cannot follow
//! Go's type flow past a constructor, so a multi-value `:=`, a method's result,
//! a chain, a range variable, an element, a type switch, a qualified `var`, an
//! embedded field and an interface all fell to "uninferred receiver".
//!
//! This module evaluates the structured facts `devmap_extract::gofacts`
//! records. Types are *identified* — `(directory, package clause, name)` for a
//! type the index holds, `(import path, name)` for one it does not — and a
//! receiver is typed by walking the expression: identifiers through the
//! lexical scope the extractor recorded, package members through the file's
//! imports, fields and methods through Go's selector rules (shallowest depth
//! wins, embedding is breadth-first, equal depth is ambiguous).
//!
//! The answer is one of:
//!
//! * a concrete method (`Concrete`) — bound at DETERMINISTIC like any typed
//!   receiver;
//! * a method an in-repo **interface** requires (`Interface`), with the
//!   in-repo types that satisfy it — the dispatch evidence;
//! * the receiver's type is **known and external** (`External`) — the method is
//!   not this repository's, so no repository symbol may be bound by name;
//! * the receiver's type is known and in-repo but declares no such member
//!   (`Absent`) — distinct from "type unknown";
//! * `Pass` — the evaluator cannot tell, and the existing ladder decides.
//!
//! Abstains rather than guesses, everywhere: a package any of whose files did
//! not parse cleanly, a type or method declared twice (build-tag variants), a
//! dot-import, an ambiguous selector, a function-local type shadowing a name.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use devmap_extract::gofacts::*;
use devmap_extract::model::{Extraction, ParseOutcome};

use super::Resolver;

/// `(directory, package clause)`: a directory holds `package foo` and its
/// external test package `foo_test`, and the two are different scopes.
pub(super) type PkgKey = (String, String);

/// Deepest embedding chain followed.
const MAX_EMBED_DEPTH: usize = 8;
/// Deepest expression evaluation (bindings evaluate bindings).
const MAX_EVAL_DEPTH: usize = 24;
/// Most in-repo implementers one interface method call fans out to as edges.
pub(super) const DISPATCH_FANOUT_CAP: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct TypeKey {
    pkg: PkgKey,
    name: String,
}

impl TypeKey {
    pub(super) fn name(&self) -> &str {
        &self.name
    }
}

/// A resolved Go type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Ty {
    /// A type the index declares.
    Named {
        key: TypeKey,
        args: Vec<Ty>,
    },
    /// A type from a package the index does not hold.
    External {
        path: String,
    },
    /// A predeclared type (`int`, `string`, `error`, `any`).
    Basic(String),
    Ptr(Box<Ty>),
    Slice(Box<Ty>),
    Array(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Chan(Box<Ty>),
    Func(Vec<Ty>, Vec<Ty>),
    Tuple(Vec<Ty>),
    /// A type parameter not yet substituted.
    Param(String),
    Unknown,
}

fn deref(ty: &Ty) -> &Ty {
    match ty {
        Ty::Ptr(inner) => inner,
        other => other,
    }
}

#[derive(Debug)]
struct TypeInfo {
    file: String,
    decl: GoTypeDecl,
}

#[derive(Debug)]
struct FuncInfo {
    file: String,
    func: GoFunc,
}

#[derive(Default, Debug)]
struct GoPkg {
    types: BTreeMap<String, Vec<TypeInfo>>,
    funcs: BTreeMap<String, Vec<FuncInfo>>,
    /// `(type name, method name)`.
    methods: BTreeMap<(String, String), Vec<FuncInfo>>,
    vars: BTreeMap<String, Vec<(String, GoBinding)>>,
}

enum ImportTarget {
    Repo(PkgKey),
    External(String),
}

/// What the evaluator makes of an expression.
enum Op<'a> {
    Val(Ty),
    /// An expression that denotes a type (`(*T)` in a method expression).
    Type(Ty),
    Pkg(ImportTargetRef),
    Fun(FunRef<'a>),
    /// An interface method value; calling it yields its declared results.
    IfaceFun(Vec<Ty>),
    Builtin(String),
    Unknown,
}

#[derive(Clone)]
enum ImportTargetRef {
    Repo(PkgKey),
    External(String),
}

#[derive(Clone)]
struct FunRef<'a> {
    info: &'a FuncInfo,
    /// The receiver type's arguments, paired with the receiver's parameter
    /// names, for a method on a generic type.
    owner: Vec<(String, Ty)>,
    /// Explicit instantiation of the function's own type parameters.
    explicit: Vec<Ty>,
}

/// A selector's answer on a type.
enum Member<'a> {
    Field(Ty),
    Method {
        info: &'a FuncInfo,
        owner: Vec<(String, Ty)>,
    },
    Interface {
        /// The interface type the selector was written on: its implementers
        /// are the candidates, even when an embedded interface declares the
        /// method.
        entry: TypeKey,
        /// The interface that declares the method, and where.
        key: TypeKey,
        file: String,
        method: GoIfaceMethod,
    },
    /// Reached only through an embedded type from outside the index.
    External(String),
    /// Complete knowledge: nothing of that name.
    Absent,
    /// Cannot tell.
    Unknown,
}

/// The evaluator's answer for one call.
pub(super) enum GoCall {
    Concrete {
        file: String,
        symbol: String,
        receiver_type: String,
    },
    Interface {
        iface: TypeKey,
        iface_file: String,
        /// The interface method's symbol (`file::Iface.Method`).
        iface_symbol: String,
        /// In-repo concrete targets of the method, deduplicated; `None` when
        /// the implementer set could not be bounded or computed.
        targets: Option<Vec<(String, String)>>,
    },
    External(String),
    Absent {
        receiver_type: String,
    },
    Pass,
}

/// Implementers per `(interface, caller is a test file)`; `None` when unbounded.
type ImplementerCache = HashMap<(TypeKey, bool), Option<Arc<Vec<TypeKey>>>>;

#[derive(Default)]
pub(super) struct GoModel {
    pkgs: BTreeMap<PkgKey, GoPkg>,
    imports: BTreeMap<String, Vec<(Option<String>, String)>>,
    resolved_imports: BTreeMap<String, OnceLock<BTreeMap<String, ImportTarget>>>,
    dot_importers: BTreeSet<String>,
    incomplete_dirs: BTreeSet<String>,
    type_method_names: Mutex<HashMap<TypeKey, Arc<BTreeSet<String>>>>,
    implementers: Mutex<ImplementerCache>,
}

impl GoModel {
    pub(super) fn index(&mut self, ext: &Extraction, dir: &str, package: &str) {
        let key: PkgKey = (dir.to_string(), package.to_string());
        let clean = matches!(ext.parse_outcome, ParseOutcome::Clean);
        let Some(facts) = ext.go_facts.as_ref() else {
            self.incomplete_dirs.insert(dir.to_string());
            return;
        };
        if !clean || facts.truncated {
            self.incomplete_dirs.insert(dir.to_string());
        }
        let imports: Vec<(Option<String>, String)> = ext
            .imports
            .iter()
            .map(|import| (import.alias.clone(), import.module_specifier.clone()))
            .collect();
        if imports
            .iter()
            .any(|(alias, _)| alias.as_deref() == Some("."))
        {
            self.dot_importers.insert(ext.file_path.clone());
        }
        self.imports.insert(ext.file_path.clone(), imports);
        self.resolved_imports
            .insert(ext.file_path.clone(), OnceLock::new());
        let pkg = self.pkgs.entry(key).or_default();
        for decl in &facts.types {
            pkg.types
                .entry(decl.name.clone())
                .or_default()
                .push(TypeInfo {
                    file: ext.file_path.clone(),
                    decl: decl.clone(),
                });
        }
        for func in &facts.funcs {
            let info = FuncInfo {
                file: ext.file_path.clone(),
                func: func.clone(),
            };
            match &func.recv {
                Some(recv) => pkg
                    .methods
                    .entry((recv.type_name.clone(), func.name.clone()))
                    .or_default()
                    .push(info),
                None => pkg.funcs.entry(func.name.clone()).or_default().push(info),
            }
        }
        for var in &facts.vars {
            pkg.vars
                .entry(var.name.clone())
                .or_default()
                .push((ext.file_path.clone(), var.binding.clone()));
        }
    }
}

/// Per-file lookup tables built once per file resolved.
pub(super) struct GoFileIndex<'a> {
    facts: &'a GoFacts,
    locals: HashMap<&'a str, Vec<&'a GoLocal>>,
    local_types: HashMap<&'a str, Vec<&'a GoLocalType>>,
    sites: HashMap<(usize, usize), &'a GoCallSite>,
}

impl<'a> GoFileIndex<'a> {
    pub(super) fn new(ext: &'a Extraction) -> Option<Self> {
        let facts = ext.go_facts.as_ref()?;
        let mut locals: HashMap<&str, Vec<&GoLocal>> = HashMap::new();
        for local in &facts.locals {
            locals.entry(local.name.as_str()).or_default().push(local);
        }
        let mut local_types: HashMap<&str, Vec<&GoLocalType>> = HashMap::new();
        for local in &facts.local_types {
            local_types
                .entry(local.name.as_str())
                .or_default()
                .push(local);
        }
        let sites = facts
            .call_sites
            .iter()
            .map(|site| ((site.start, site.end), site))
            .collect();
        Some(Self {
            facts,
            locals,
            local_types,
            sites,
        })
    }

    fn local_at(&self, name: &str, pos: usize) -> Option<&'a GoLocal> {
        self.locals
            .get(name)?
            .iter()
            .filter(|local| local.from <= pos && pos < local.to)
            .max_by_key(|local| local.from)
            .copied()
    }

    fn local_type_at(&self, name: &str, pos: usize) -> bool {
        self.local_types
            .get(name)
            .is_some_and(|types| types.iter().any(|ty| ty.from <= pos && pos < ty.to))
    }

    /// The function declaration containing `pos`.
    fn func_at(&self, pos: usize) -> Option<&'a GoFunc> {
        let funcs = &self.facts.funcs;
        let after = funcs.partition_point(|func| func.start <= pos);
        let candidate = funcs.get(after.checked_sub(1)?)?;
        (pos < candidate.end).then_some(candidate)
    }
}

/// Where an expression is evaluated: a file, a position in it, and the type
/// parameters in scope.
struct Cx<'a> {
    file: &'a str,
    pos: usize,
    index: Option<&'a GoFileIndex<'a>>,
    tparams: Vec<String>,
}

impl<'a> Cx<'a> {
    /// The context of a declaration in another file: no lexical scope.
    fn decl(file: &'a str, tparams: Vec<String>) -> Self {
        Cx {
            file,
            pos: 0,
            index: None,
            tparams,
        }
    }
}

const UNIVERSE_TYPES: &[&str] = &[
    "bool",
    "byte",
    "comparable",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "any",
];

const UNIVERSE_FUNCS: &[&str] = &[
    "append", "cap", "clear", "close", "complex", "copy", "delete", "imag", "len", "make", "max",
    "min", "new", "panic", "print", "println", "real", "recover",
];

impl Resolver {
    fn go_dir_incomplete(&self, dir: &str) -> bool {
        self.go_model.incomplete_dirs.contains(dir) || self.go_package_dirs_unvouched.contains(dir)
    }

    fn go_pkg_of_file(&self, file: &str) -> Option<PkgKey> {
        let package = self.go_package_by_file.get(file)?;
        Some((Self::parent_dir(file), package.clone()))
    }

    /// The imports of `file`, local name to target, resolved once.
    fn go_imports(&self, file: &str) -> Option<&BTreeMap<String, ImportTarget>> {
        let cell = self.go_model.resolved_imports.get(file)?;
        Some(cell.get_or_init(|| {
            let mut out = BTreeMap::new();
            for (alias, spec) in self.go_model.imports.get(file).into_iter().flatten() {
                if matches!(alias.as_deref(), Some("_") | Some(".")) {
                    continue;
                }
                let resolved = self.resolve_go_import(spec);
                let repo_key = resolved
                    .iter()
                    .find_map(|target| self.go_pkg_of_file(target));
                let (local, target) = match repo_key {
                    Some(key) => (
                        alias.clone().unwrap_or_else(|| key.1.clone()),
                        ImportTarget::Repo(key),
                    ),
                    None => {
                        let last = spec.rsplit('/').next().unwrap_or(spec);
                        // `example.com/mod/v2` is imported as `mod`.
                        let last = if last.len() > 1
                            && last.starts_with('v')
                            && last[1..].chars().all(|c| c.is_ascii_digit())
                        {
                            spec.rsplit('/').nth(1).unwrap_or(last)
                        } else {
                            last
                        };
                        (
                            alias.clone().unwrap_or_else(|| last.to_string()),
                            ImportTarget::External(spec.clone()),
                        )
                    }
                };
                out.insert(local, target);
            }
            out
        }))
    }

    fn go_import_ref(&self, file: &str, name: &str) -> Option<ImportTargetRef> {
        Some(match self.go_imports(file)?.get(name)? {
            ImportTarget::Repo(key) => ImportTargetRef::Repo(key.clone()),
            ImportTarget::External(path) => ImportTargetRef::External(path.clone()),
        })
    }

    /// The single declaration of `name` in `pkg`, visible from `from`.
    fn go_type_info(&self, pkg: &PkgKey, name: &str, from: &str) -> Option<&TypeInfo> {
        let hits = self.go_model.pkgs.get(pkg)?.types.get(name)?;
        let from_test = from.ends_with("_test.go");
        let mut visible = hits
            .iter()
            .filter(|info| from_test || !info.file.ends_with("_test.go"));
        let first = visible.next()?;
        visible.next().is_none().then_some(first)
    }

    // ---------------------------------------------------------------- types

    fn go_ty(&self, cx: &Cx, ty: &GoType, depth: usize) -> Ty {
        if depth > MAX_EVAL_DEPTH {
            return Ty::Unknown;
        }
        match ty {
            GoType::Name { pkg, name, args } => {
                let resolved_args = || -> Vec<Ty> {
                    args.iter()
                        .map(|arg| self.go_ty(cx, arg, depth + 1))
                        .collect()
                };
                match pkg {
                    Some(qualifier) => {
                        // A local shadows an import of the same name.
                        if cx
                            .index
                            .is_some_and(|index| index.local_at(qualifier, cx.pos).is_some())
                        {
                            return Ty::Unknown;
                        }
                        match self.go_import_ref(cx.file, qualifier) {
                            Some(ImportTargetRef::Repo(key)) => {
                                self.go_named(cx, &key, name, resolved_args(), depth)
                            }
                            Some(ImportTargetRef::External(path)) => Ty::External { path },
                            None => Ty::Unknown,
                        }
                    }
                    None => {
                        if cx.tparams.iter().any(|param| param == name) {
                            return Ty::Param(name.clone());
                        }
                        if cx
                            .index
                            .is_some_and(|index| index.local_type_at(name, cx.pos))
                        {
                            return Ty::Unknown;
                        }
                        if let Some(key) = self.go_pkg_of_file(cx.file) {
                            if self
                                .go_model
                                .pkgs
                                .get(&key)
                                .is_some_and(|pkg| pkg.types.contains_key(name.as_str()))
                            {
                                return self.go_named(cx, &key, name, resolved_args(), depth);
                            }
                        }
                        if UNIVERSE_TYPES.contains(&name.as_str()) {
                            return Ty::Basic(name.clone());
                        }
                        Ty::Unknown
                    }
                }
            }
            GoType::Ptr(inner) => Ty::Ptr(Box::new(self.go_ty(cx, inner, depth + 1))),
            GoType::Slice(inner) => Ty::Slice(Box::new(self.go_ty(cx, inner, depth + 1))),
            GoType::Array(inner) => Ty::Array(Box::new(self.go_ty(cx, inner, depth + 1))),
            GoType::Chan(inner) => Ty::Chan(Box::new(self.go_ty(cx, inner, depth + 1))),
            GoType::Map(key, value) => Ty::Map(
                Box::new(self.go_ty(cx, key, depth + 1)),
                Box::new(self.go_ty(cx, value, depth + 1)),
            ),
            GoType::Func { params, results } => Ty::Func(
                params
                    .iter()
                    .map(|param| self.go_ty(cx, param, depth + 1))
                    .collect(),
                results
                    .iter()
                    .map(|result| self.go_ty(cx, result, depth + 1))
                    .collect(),
            ),
            GoType::Opaque | GoType::Unknown => Ty::Unknown,
        }
    }

    /// A named type of `pkg`, with an alias followed to what it denotes.
    fn go_named(&self, cx: &Cx, pkg: &PkgKey, name: &str, args: Vec<Ty>, depth: usize) -> Ty {
        let Some(info) = self.go_type_info(pkg, name, cx.file) else {
            return Ty::Unknown;
        };
        if let GoTypeBody::Alias(target) = &info.decl.body {
            let decl_cx = Cx::decl(&info.file, info.decl.type_params.clone());
            return self.go_ty(&decl_cx, target, depth + 1);
        }
        Ty::Named {
            key: TypeKey {
                pkg: pkg.clone(),
                name: name.to_string(),
            },
            args,
        }
    }

    fn go_subst(&self, ty: Ty, map: &[(String, Ty)]) -> Ty {
        if map.is_empty() {
            return ty;
        }
        match ty {
            Ty::Param(name) => map
                .iter()
                .find(|(param, _)| *param == name)
                .map(|(_, bound)| bound.clone())
                .unwrap_or(Ty::Param(name)),
            Ty::Named { key, args } => Ty::Named {
                key,
                args: args.into_iter().map(|a| self.go_subst(a, map)).collect(),
            },
            Ty::Ptr(i) => Ty::Ptr(Box::new(self.go_subst(*i, map))),
            Ty::Slice(i) => Ty::Slice(Box::new(self.go_subst(*i, map))),
            Ty::Array(i) => Ty::Array(Box::new(self.go_subst(*i, map))),
            Ty::Chan(i) => Ty::Chan(Box::new(self.go_subst(*i, map))),
            Ty::Map(k, v) => Ty::Map(
                Box::new(self.go_subst(*k, map)),
                Box::new(self.go_subst(*v, map)),
            ),
            Ty::Func(p, r) => Ty::Func(
                p.into_iter().map(|t| self.go_subst(t, map)).collect(),
                r.into_iter().map(|t| self.go_subst(t, map)).collect(),
            ),
            Ty::Tuple(items) => {
                Ty::Tuple(items.into_iter().map(|t| self.go_subst(t, map)).collect())
            }
            other => other,
        }
    }

    // -------------------------------------------------------------- members

    /// Go's selector rule on `start`: the member named `name` at the shallowest
    /// embedding depth, ambiguous (so abstained on) when two share that depth.
    fn go_member<'a>(&'a self, start: &Ty, name: &str, from: &str) -> Member<'a> {
        let mut level: Vec<Ty> = vec![deref(start).clone()];
        let mut seen: HashSet<TypeKey> = HashSet::new();
        let mut external: Option<String> = None;
        let mut incomplete = false;
        for _ in 0..=MAX_EMBED_DEPTH {
            let mut hits: Vec<Member<'a>> = Vec::new();
            let mut next: Vec<Ty> = Vec::new();
            for ty in &level {
                match ty {
                    Ty::External { path } => external = external.take().or(Some(path.clone())),
                    Ty::Named { key, args } => {
                        if !seen.insert(key.clone()) {
                            continue;
                        }
                        if self.go_dir_incomplete(&key.pkg.0) {
                            incomplete = true;
                        }
                        let Some(info) = self.go_type_info(&key.pkg, &key.name, from) else {
                            incomplete = true;
                            continue;
                        };
                        let owner: Vec<(String, Ty)> = info
                            .decl
                            .type_params
                            .iter()
                            .cloned()
                            .zip(args.iter().cloned())
                            .collect();
                        let Some(pkg) = self.go_model.pkgs.get(&key.pkg) else {
                            incomplete = true;
                            continue;
                        };
                        // A `_test.go` method is not part of the type a non-test
                        // file sees.
                        let from_test = from.ends_with("_test.go");
                        let declared: Vec<&FuncInfo> = pkg
                            .methods
                            .get(&(key.name.clone(), name.to_string()))
                            .into_iter()
                            .flatten()
                            .filter(|method| from_test || !method.file.ends_with("_test.go"))
                            .collect();
                        match declared.as_slice() {
                            [] => {}
                            [method] => hits.push(Member::Method {
                                info: method,
                                owner: method
                                    .func
                                    .recv
                                    .as_ref()
                                    .map(|recv| {
                                        recv.type_params
                                            .iter()
                                            .cloned()
                                            .zip(args.iter().cloned())
                                            .collect()
                                    })
                                    .unwrap_or_default(),
                            }),
                            _ => incomplete = true,
                        }
                        let decl_cx = Cx::decl(&info.file, info.decl.type_params.clone());
                        match &info.decl.body {
                            GoTypeBody::Struct(fields) => {
                                self.go_struct_level(
                                    fields, &decl_cx, &owner, name, &mut hits, &mut next,
                                );
                            }
                            GoTypeBody::Interface { .. } => {
                                match self.go_iface_method(key, name, from) {
                                    IfaceLookup::Found(found_key, file, method) => {
                                        hits.push(Member::Interface {
                                            entry: key.clone(),
                                            key: found_key,
                                            file,
                                            method,
                                        })
                                    }
                                    IfaceLookup::External(path) => {
                                        external = external.take().or(Some(path))
                                    }
                                    IfaceLookup::Incomplete => incomplete = true,
                                    IfaceLookup::Absent => {}
                                }
                            }
                            GoTypeBody::Defined(underlying) => {
                                // The underlying struct's fields (and what they
                                // embed) are this type's own; its methods are not.
                                if let Ty::Named { key: under, .. } =
                                    self.go_ty(&decl_cx, underlying, 0)
                                {
                                    if let Some(under_info) =
                                        self.go_type_info(&under.pkg, &under.name, from)
                                    {
                                        if let GoTypeBody::Struct(fields) = &under_info.decl.body {
                                            let under_cx = Cx::decl(
                                                &under_info.file,
                                                under_info.decl.type_params.clone(),
                                            );
                                            self.go_struct_level(
                                                fields,
                                                &under_cx,
                                                &[],
                                                name,
                                                &mut hits,
                                                &mut next,
                                            );
                                        }
                                    }
                                }
                            }
                            GoTypeBody::Alias(_) => {}
                        }
                    }
                    _ => {}
                }
            }
            match hits.len() {
                0 => {}
                1 => return hits.into_iter().next().unwrap_or(Member::Unknown),
                // Two members at one depth: the program does not compile, or
                // the facts are wrong. Either way, no answer.
                _ => return Member::Unknown,
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
        if incomplete {
            return Member::Unknown;
        }
        match external {
            Some(path) => Member::External(path),
            None => Member::Absent,
        }
    }

    fn go_struct_level<'a>(
        &'a self,
        fields: &[GoField],
        decl_cx: &Cx,
        owner: &[(String, Ty)],
        name: &str,
        hits: &mut Vec<Member<'a>>,
        next: &mut Vec<Ty>,
    ) {
        for field in fields {
            if field.name == name {
                let ty = self.go_ty(decl_cx, &field.ty, 0);
                hits.push(Member::Field(self.go_subst(ty, owner)));
            }
            if field.embedded {
                let ty = self.go_ty(decl_cx, &field.ty, 0);
                next.push(deref(&self.go_subst(ty, owner)).clone());
            }
        }
    }

    /// An interface's method `name`, searched through its embedded interfaces.
    fn go_iface_method(&self, iface: &TypeKey, name: &str, from: &str) -> IfaceLookup {
        let mut pending = vec![iface.clone()];
        let mut seen: HashSet<TypeKey> = HashSet::new();
        let mut external: Option<String> = None;
        let mut incomplete = false;
        while let Some(key) = pending.pop() {
            if !seen.insert(key.clone()) || seen.len() > 64 {
                continue;
            }
            let Some(info) = self.go_type_info(&key.pkg, &key.name, from) else {
                incomplete = true;
                continue;
            };
            let GoTypeBody::Interface { methods, embeds } = &info.decl.body else {
                continue;
            };
            if let Some(method) = methods.iter().find(|method| method.name == name) {
                return IfaceLookup::Found(key, info.file.clone(), method.clone());
            }
            let decl_cx = Cx::decl(&info.file, info.decl.type_params.clone());
            for embed in embeds {
                match self.go_ty(&decl_cx, embed, 0) {
                    Ty::Named { key, .. } => pending.push(key),
                    Ty::External { path } => external = external.take().or(Some(path)),
                    Ty::Basic(ref basic) if basic == "error" => {
                        if name == "Error" {
                            return IfaceLookup::External("error".to_string());
                        }
                    }
                    _ => incomplete = true,
                }
            }
        }
        if incomplete {
            IfaceLookup::Incomplete
        } else if let Some(path) = external {
            IfaceLookup::External(path)
        } else {
            IfaceLookup::Absent
        }
    }

    /// Every method an interface requires that the index can see, and whether
    /// an embedded interface from outside the index hides more.
    fn go_iface_requirements(
        &self,
        iface: &TypeKey,
        from: &str,
    ) -> Option<(Vec<GoIfaceMethod>, bool)> {
        let mut pending = vec![iface.clone()];
        let mut seen: HashSet<TypeKey> = HashSet::new();
        let mut out: Vec<GoIfaceMethod> = Vec::new();
        let mut partial = false;
        while let Some(key) = pending.pop() {
            if !seen.insert(key.clone()) || seen.len() > 64 {
                continue;
            }
            let info = self.go_type_info(&key.pkg, &key.name, from)?;
            let GoTypeBody::Interface { methods, embeds } = &info.decl.body else {
                return None;
            };
            for method in methods {
                if !out.iter().any(|known| known.name == method.name) {
                    out.push(method.clone());
                }
            }
            let decl_cx = Cx::decl(&info.file, info.decl.type_params.clone());
            for embed in embeds {
                match self.go_ty(&decl_cx, embed, 0) {
                    Ty::Named { key, .. } => pending.push(key),
                    Ty::Basic(ref basic) if basic == "error" => {
                        let error = GoIfaceMethod {
                            name: "Error".to_string(),
                            params: Vec::new(),
                            results: vec![GoType::Name {
                                pkg: None,
                                name: "string".to_string(),
                                args: Vec::new(),
                            }],
                        };
                        if !out.iter().any(|known| known.name == "Error") {
                            out.push(error);
                        }
                    }
                    _ => partial = true,
                }
            }
        }
        Some((out, partial))
    }

    /// Method names `ty` has, declared or promoted.
    fn go_method_names(&self, key: &TypeKey, from: &str) -> Arc<BTreeSet<String>> {
        if let Some(cached) = self
            .go_model
            .type_method_names
            .lock()
            .ok()
            .and_then(|cache| cache.get(key).cloned())
        {
            return cached;
        }
        let mut names = BTreeSet::new();
        let mut seen: HashSet<TypeKey> = HashSet::new();
        let mut level = vec![key.clone()];
        for _ in 0..=MAX_EMBED_DEPTH {
            let mut next = Vec::new();
            for current in level {
                if !seen.insert(current.clone()) {
                    continue;
                }
                let Some(pkg) = self.go_model.pkgs.get(&current.pkg) else {
                    continue;
                };
                for (type_name, method) in pkg.methods.keys() {
                    if *type_name == current.name {
                        names.insert(method.clone());
                    }
                }
                let Some(info) = self.go_type_info(&current.pkg, &current.name, from) else {
                    continue;
                };
                let decl_cx = Cx::decl(&info.file, info.decl.type_params.clone());
                match &info.decl.body {
                    GoTypeBody::Struct(fields) => {
                        for field in fields.iter().filter(|field| field.embedded) {
                            if let Ty::Named { key, .. } =
                                deref(&self.go_ty(&decl_cx, &field.ty, 0)).clone()
                            {
                                next.push(key);
                            }
                        }
                    }
                    GoTypeBody::Interface { .. } => {
                        if let Some((methods, _)) = self.go_iface_requirements(&current, from) {
                            names.extend(methods.into_iter().map(|method| method.name));
                        }
                    }
                    _ => {}
                }
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
        let names = Arc::new(names);
        if let Ok(mut cache) = self.go_model.type_method_names.lock() {
            cache.insert(key.clone(), Arc::clone(&names));
        }
        names
    }

    /// The in-repo concrete types that satisfy `iface`, or `None` when the
    /// interface constrains too little to bound them (no known method) or
    /// cannot be read.
    ///
    /// Satisfaction is by method name and arity — parameter and result *counts* —
    /// over the pointer method set, embedding included. It does not compare
    /// parameter types, so it can over-include a type whose same-named,
    /// same-arity method differs in type; that is the documented precision
    /// bound of a dispatch edge. An interface with an unexported method is
    /// satisfied only by types of its own package, as Go requires.
    fn go_implementers(&self, iface: &TypeKey, from: &str) -> Option<Arc<Vec<TypeKey>>> {
        let from_test = from.ends_with("_test.go");
        if let Some(cached) = self
            .go_model
            .implementers
            .lock()
            .ok()
            .and_then(|cache| cache.get(&(iface.clone(), from_test)).cloned())
        {
            return cached;
        }
        let computed = self.compute_go_implementers(iface, from).map(Arc::new);
        if let Ok(mut cache) = self.go_model.implementers.lock() {
            cache.insert((iface.clone(), from_test), computed.clone());
        }
        computed
    }

    fn compute_go_implementers(&self, iface: &TypeKey, from: &str) -> Option<Vec<TypeKey>> {
        let (required, _partial) = self.go_iface_requirements(iface, from)?;
        if required.is_empty() {
            return None;
        }
        let same_package_only = required
            .iter()
            .any(|method| Self::go_name_is_unexported(&method.name));
        let from_test = from.ends_with("_test.go");
        let mut out = Vec::new();
        for (pkg_key, pkg) in &self.go_model.pkgs {
            if same_package_only && *pkg_key != iface.pkg {
                continue;
            }
            for (name, infos) in &pkg.types {
                let [info] = infos.as_slice() else { continue };
                if !matches!(
                    info.decl.body,
                    GoTypeBody::Struct(_) | GoTypeBody::Defined(_)
                ) {
                    continue;
                }
                if !from_test && info.file.ends_with("_test.go") {
                    continue;
                }
                let key = TypeKey {
                    pkg: pkg_key.clone(),
                    name: name.clone(),
                };
                let have = self.go_method_names(&key, from);
                if !required.iter().all(|method| have.contains(&method.name)) {
                    continue;
                }
                let as_pointer = Ty::Ptr(Box::new(Ty::Named {
                    key: key.clone(),
                    args: Vec::new(),
                }));
                let satisfied = required.iter().all(|method| {
                    match self.go_member(&as_pointer, &method.name, from) {
                        Member::Method { info, .. } => {
                            info.func.params.len() == method.params.len()
                                && info.func.results.len() == method.results.len()
                        }
                        _ => false,
                    }
                });
                if satisfied {
                    out.push(key);
                }
            }
        }
        out.sort();
        Some(out)
    }

    // ---------------------------------------------------------- expressions

    fn go_pkg_member<'a>(&'a self, pkg: &PkgKey, name: &str, from: &str) -> Op<'a> {
        let Some(go_pkg) = self.go_model.pkgs.get(pkg) else {
            return Op::Unknown;
        };
        let from_test = from.ends_with("_test.go");
        let visible = |file: &str| from_test || !file.ends_with("_test.go");
        if let Some(funcs) = go_pkg.funcs.get(name) {
            let mut candidates = funcs.iter().filter(|info| visible(&info.file));
            return match (candidates.next(), candidates.next()) {
                (Some(info), None) => Op::Fun(FunRef {
                    info,
                    owner: Vec::new(),
                    explicit: Vec::new(),
                }),
                _ => Op::Unknown,
            };
        }
        if let Some(vars) = go_pkg.vars.get(name) {
            let mut candidates = vars.iter().filter(|(file, _)| visible(file));
            return match (candidates.next(), candidates.next()) {
                (Some((file, binding)), None) => {
                    let cx = Cx::decl(file, Vec::new());
                    match self.go_binding_ty(&cx, binding, 0) {
                        Some(ty) => Op::Val(ty),
                        None => Op::Unknown,
                    }
                }
                _ => Op::Unknown,
            };
        }
        if go_pkg.types.contains_key(name) {
            let cx = Cx::decl(from, Vec::new());
            return match self.go_named(&cx, pkg, name, Vec::new(), 0) {
                Ty::Unknown => Op::Unknown,
                ty => Op::Type(ty),
            };
        }
        Op::Unknown
    }

    fn go_eval<'a>(&'a self, cx: &Cx, expr: &GoExpr, depth: usize) -> Op<'a> {
        if depth > MAX_EVAL_DEPTH {
            return Op::Unknown;
        }
        match expr {
            GoExpr::Ident(name) => self.go_eval_ident(cx, name, depth),
            GoExpr::Selector(base, name) => {
                let base = self.go_eval(cx, base, depth + 1);
                self.go_eval_selector(cx, base, name)
            }
            GoExpr::Call { fun, targs, arg0 } => {
                let fun = self.go_eval(cx, fun, depth + 1);
                self.go_eval_call(cx, fun, targs, arg0.as_deref(), depth)
            }
            GoExpr::Index(base, targs) => match self.go_eval(cx, base, depth + 1) {
                Op::Fun(mut fun) => {
                    fun.explicit = targs.iter().map(|t| self.go_ty(cx, t, 0)).collect();
                    Op::Fun(fun)
                }
                Op::Type(Ty::Named { key, .. }) => Op::Type(Ty::Named {
                    key,
                    args: targs.iter().map(|t| self.go_ty(cx, t, 0)).collect(),
                }),
                Op::Val(ty) => match self.go_element(&ty) {
                    Some(element) => Op::Val(element),
                    None => Op::Unknown,
                },
                _ => Op::Unknown,
            },
            GoExpr::Slice(base) => match self.go_eval(cx, base, depth + 1) {
                Op::Val(ty) => match ty {
                    Ty::Slice(_) | Ty::Basic(_) => Op::Val(ty),
                    Ty::Array(inner) => Op::Val(Ty::Slice(inner)),
                    Ty::Ptr(inner) => match *inner {
                        Ty::Array(element) => Op::Val(Ty::Slice(element)),
                        _ => Op::Unknown,
                    },
                    _ => Op::Unknown,
                },
                _ => Op::Unknown,
            },
            GoExpr::Star(base) => match self.go_eval(cx, base, depth + 1) {
                Op::Type(ty) => Op::Type(Ty::Ptr(Box::new(ty))),
                Op::Val(Ty::Ptr(inner)) => Op::Val(*inner),
                _ => Op::Unknown,
            },
            GoExpr::Addr(base) => match self.go_eval(cx, base, depth + 1) {
                Op::Val(ty) => Op::Val(Ty::Ptr(Box::new(ty))),
                _ => Op::Unknown,
            },
            GoExpr::Recv(base) => match self.go_eval(cx, base, depth + 1) {
                Op::Val(Ty::Chan(element)) => Op::Val(*element),
                _ => Op::Unknown,
            },
            GoExpr::Composite(ty) | GoExpr::Assert(ty) => match self.go_ty(cx, ty, 0) {
                Ty::Unknown => Op::Unknown,
                ty => Op::Val(ty),
            },
            GoExpr::Type(ty) => match self.go_ty(cx, ty, 0) {
                Ty::Unknown => Op::Unknown,
                ty => Op::Type(ty),
            },
            GoExpr::FuncLit { params, results } => Op::Val(Ty::Func(
                params.iter().map(|t| self.go_ty(cx, t, 0)).collect(),
                results.iter().map(|t| self.go_ty(cx, t, 0)).collect(),
            )),
            GoExpr::Unknown => Op::Unknown,
        }
    }

    fn go_eval_ident<'a>(&'a self, cx: &Cx, name: &str, depth: usize) -> Op<'a> {
        if let Some(index) = cx.index {
            if let Some(local) = index.local_at(name, cx.pos) {
                // The binding is evaluated where it was declared: names in its
                // own initializer do not see the variable it introduces.
                let inner = Cx {
                    file: cx.file,
                    pos: local.from.saturating_sub(1),
                    index: cx.index,
                    tparams: cx.tparams.clone(),
                };
                return match self.go_binding_ty(&inner, &local.binding, depth + 1) {
                    Some(ty) => Op::Val(ty),
                    None => Op::Unknown,
                };
            }
            if index.local_type_at(name, cx.pos) {
                return Op::Unknown;
            }
        }
        if let Some(import) = self.go_import_ref(cx.file, name) {
            return Op::Pkg(import);
        }
        if let Some(pkg) = self.go_pkg_of_file(cx.file) {
            let member = self.go_pkg_member(&pkg, name, cx.file);
            if !matches!(member, Op::Unknown) {
                return member;
            }
            if self.go_model.pkgs.get(&pkg).is_some_and(|go_pkg| {
                go_pkg.types.contains_key(name) || go_pkg.funcs.contains_key(name)
            }) {
                // Declared in this package and not readable (ambiguous, or a
                // test-only declaration): do not fall to the universe.
                return Op::Unknown;
            }
        }
        if UNIVERSE_TYPES.contains(&name) {
            return Op::Type(Ty::Basic(name.to_string()));
        }
        if UNIVERSE_FUNCS.contains(&name) {
            return Op::Builtin(name.to_string());
        }
        Op::Unknown
    }

    fn go_eval_selector<'a>(&'a self, cx: &Cx, base: Op<'a>, name: &str) -> Op<'a> {
        match base {
            Op::Pkg(ImportTargetRef::Repo(key)) => self.go_pkg_member(&key, name, cx.file),
            Op::Pkg(ImportTargetRef::External(_)) => Op::Unknown,
            Op::Val(ty) | Op::Type(ty) => match self.go_member(&ty, name, cx.file) {
                Member::Field(field) => Op::Val(field),
                Member::Method { info, owner, .. } => Op::Fun(FunRef {
                    info,
                    owner,
                    explicit: Vec::new(),
                }),
                Member::Interface { file, method, .. } => {
                    let decl_cx = Cx::decl(&file, Vec::new());
                    Op::IfaceFun(
                        method
                            .results
                            .iter()
                            .map(|result| self.go_ty(&decl_cx, result, 0))
                            .collect(),
                    )
                }
                _ => Op::Unknown,
            },
            _ => Op::Unknown,
        }
    }

    fn go_eval_call<'a>(
        &'a self,
        cx: &Cx,
        fun: Op<'a>,
        targs: &[GoType],
        arg0: Option<&GoExpr>,
        depth: usize,
    ) -> Op<'a> {
        match fun {
            Op::Fun(mut fun) => {
                if !targs.is_empty() {
                    fun.explicit = targs.iter().map(|t| self.go_ty(cx, t, 0)).collect();
                }
                let results = self.go_fun_results(&fun);
                Self::go_pack(results)
            }
            Op::IfaceFun(results) => Self::go_pack(results),
            Op::Type(ty) => Op::Val(ty),
            Op::Val(Ty::Func(_, results)) => Self::go_pack(results),
            Op::Builtin(name) => {
                let Some(first) = arg0 else {
                    return Op::Unknown;
                };
                match (name.as_str(), self.go_eval(cx, first, depth + 1)) {
                    ("new", Op::Type(ty)) => Op::Val(Ty::Ptr(Box::new(ty))),
                    ("make", Op::Type(ty)) => Op::Val(ty),
                    ("append", Op::Val(ty)) => Op::Val(ty),
                    _ => Op::Unknown,
                }
            }
            _ => Op::Unknown,
        }
    }

    fn go_pack<'a>(mut results: Vec<Ty>) -> Op<'a> {
        match results.len() {
            0 => Op::Unknown,
            1 => Op::Val(results.remove(0)),
            _ => Op::Val(Ty::Tuple(results)),
        }
    }

    /// A function's declared results, in the context of its own declaration,
    /// with its receiver's and its own explicit type arguments substituted.
    fn go_fun_results(&self, fun: &FunRef) -> Vec<Ty> {
        let mut tparams = fun.info.func.type_params.clone();
        if let Some(recv) = &fun.info.func.recv {
            tparams.extend(recv.type_params.iter().cloned());
        }
        let decl_cx = Cx::decl(&fun.info.file, tparams);
        let mut map = fun.owner.clone();
        map.extend(
            fun.info
                .func
                .type_params
                .iter()
                .cloned()
                .zip(fun.explicit.iter().cloned()),
        );
        fun.info
            .func
            .results
            .iter()
            .map(|result| self.go_subst(self.go_ty(&decl_cx, result, 0), &map))
            .collect()
    }

    /// The element type indexing `ty` yields.
    fn go_element(&self, ty: &Ty) -> Option<Ty> {
        match deref(ty) {
            Ty::Slice(element) | Ty::Array(element) => Some((**element).clone()),
            Ty::Map(_, value) => Some((**value).clone()),
            Ty::Basic(name) if name == "string" => Some(Ty::Basic("byte".to_string())),
            Ty::Named { key, .. } => {
                // `type Stores []*Store`: indexing sees the underlying type.
                let info = self.go_type_info(&key.pkg, &key.name, "")?;
                let GoTypeBody::Defined(underlying) = &info.decl.body else {
                    return None;
                };
                let decl_cx = Cx::decl(&info.file, info.decl.type_params.clone());
                self.go_element(&self.go_ty(&decl_cx, underlying, 0))
            }
            _ => None,
        }
    }

    /// The key and value types ranging over `ty` yields.
    fn go_range(&self, ty: &Ty) -> Option<(Ty, Option<Ty>)> {
        match deref(ty) {
            Ty::Slice(element) | Ty::Array(element) => {
                Some((Ty::Basic("int".to_string()), Some((**element).clone())))
            }
            Ty::Map(key, value) => Some(((**key).clone(), Some((**value).clone()))),
            Ty::Chan(element) => Some(((**element).clone(), None)),
            Ty::Basic(name) if name == "string" => Some((
                Ty::Basic("int".to_string()),
                Some(Ty::Basic("rune".to_string())),
            )),
            Ty::Basic(name) if name.starts_with("int") || name.starts_with("uint") => {
                Some((Ty::Basic(name.clone()), None))
            }
            Ty::Named { key, .. } => {
                let info = self.go_type_info(&key.pkg, &key.name, "")?;
                let GoTypeBody::Defined(underlying) = &info.decl.body else {
                    return None;
                };
                let decl_cx = Cx::decl(&info.file, info.decl.type_params.clone());
                self.go_range(&self.go_ty(&decl_cx, underlying, 0))
            }
            _ => None,
        }
    }

    fn go_binding_ty(&self, cx: &Cx, binding: &GoBinding, depth: usize) -> Option<Ty> {
        if depth > MAX_EVAL_DEPTH {
            return None;
        }
        let known = |ty: Ty| (!matches!(ty, Ty::Unknown)).then_some(ty);
        match binding {
            GoBinding::Type(ty) => known(self.go_ty(cx, ty, depth + 1)),
            GoBinding::Expr(expr) => match self.go_eval(cx, expr, depth + 1) {
                Op::Val(Ty::Tuple(_)) => None,
                Op::Val(ty) => known(ty),
                _ => None,
            },
            GoBinding::Tuple { source, index } => {
                // `v, ok := m[k]` / `x.(T)` / `<-ch`: the comma-ok forms.
                if matches!(
                    source,
                    GoExpr::Index(..) | GoExpr::Assert(_) | GoExpr::Recv(_)
                ) {
                    return match index {
                        0 => match self.go_eval(cx, source, depth + 1) {
                            Op::Val(ty) => known(ty),
                            _ => None,
                        },
                        1 => Some(Ty::Basic("bool".to_string())),
                        _ => None,
                    };
                }
                match self.go_eval(cx, source, depth + 1) {
                    Op::Val(Ty::Tuple(items)) => items.into_iter().nth(*index).and_then(known),
                    _ => None,
                }
            }
            GoBinding::Range { over, key } => {
                let Op::Val(ty) = self.go_eval(cx, over, depth + 1) else {
                    return None;
                };
                let (key_ty, value_ty) = self.go_range(&ty)?;
                if *key {
                    known(key_ty)
                } else {
                    value_ty.and_then(known)
                }
            }
        }
    }

    // ---------------------------------------------------------------- calls

    /// What `call` (a method-style call on a Go receiver) names, as far as the
    /// type evaluator can tell. See the module documentation for the cases.
    pub(super) fn go_call(
        &self,
        ext: &Extraction,
        index: &GoFileIndex,
        call: &devmap_extract::model::ExtractedCall,
    ) -> GoCall {
        // A dot-import puts another package's exported names in this file's
        // scope: a bare type name may be that package's, so nothing here can
        // be identified.
        if self.go_model.dot_importers.contains(&ext.file_path) {
            return GoCall::Pass;
        }
        let Some(site) = index
            .sites
            .get(&(call.span.start_byte, call.span.end_byte))
            .filter(|site| site.name == call.callee_name)
        else {
            return GoCall::Pass;
        };
        let mut tparams = Vec::new();
        if let Some(func) = index.func_at(call.span.start_byte) {
            tparams.extend(func.type_params.iter().cloned());
            if let Some(recv) = &func.recv {
                tparams.extend(recv.type_params.iter().cloned());
            }
        }
        let cx = Cx {
            file: &ext.file_path,
            pos: call.span.start_byte,
            index: Some(index),
            tparams,
        };
        let receiver = match self.go_eval(&cx, &site.recv, 0) {
            Op::Val(ty) | Op::Type(ty) => ty,
            _ => return GoCall::Pass,
        };
        let described = |ty: &Ty| match deref(ty) {
            Ty::Named { key, .. } => key.name.clone(),
            _ => String::new(),
        };
        match self.go_member(&receiver, &call.callee_name, &ext.file_path) {
            Member::Method { info, .. } => GoCall::Concrete {
                file: info.file.clone(),
                symbol: info.func.qualified_name.clone(),
                receiver_type: described(&receiver),
            },
            Member::Interface {
                entry,
                key,
                file,
                method,
            } => {
                let iface_symbol = format!("{file}::{}.{}", key.name, method.name);
                let targets = self.go_dispatch_targets(&entry, &method, &ext.file_path);
                GoCall::Interface {
                    iface: entry,
                    iface_file: file,
                    iface_symbol,
                    targets,
                }
            }
            Member::External(path) => GoCall::External(path),
            Member::Absent => match deref(&receiver) {
                Ty::Named { key, .. } => GoCall::Absent {
                    receiver_type: key.name.clone(),
                },
                _ => GoCall::Pass,
            },
            Member::Field(_) | Member::Unknown => GoCall::Pass,
        }
    }

    /// The concrete methods an interface method call may reach: one per distinct
    /// declaration among the implementers.
    fn go_dispatch_targets(
        &self,
        declaring: &TypeKey,
        method: &GoIfaceMethod,
        from: &str,
    ) -> Option<Vec<(String, String)>> {
        let implementers = self.go_implementers(declaring, from)?;
        let mut targets: BTreeSet<(String, String)> = BTreeSet::new();
        for key in implementers.iter() {
            let as_pointer = Ty::Ptr(Box::new(Ty::Named {
                key: key.clone(),
                args: Vec::new(),
            }));
            if let Member::Method { info, .. } = self.go_member(&as_pointer, &method.name, from) {
                targets.insert((info.file.clone(), info.func.qualified_name.clone()));
            }
        }
        Some(targets.into_iter().collect())
    }
}

enum IfaceLookup {
    Found(TypeKey, String, GoIfaceMethod),
    External(String),
    Incomplete,
    Absent,
}
