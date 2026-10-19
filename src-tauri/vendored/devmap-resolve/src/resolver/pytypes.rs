//! Python class identity at a call site: which class a written name denotes,
//! and which declaration a method call on it reaches.
//!
//! `type_methods` and `supertypes` are flat by bare class name, so two files
//! that each declare an `Engine` collapse into one key and a call on either
//! looks ambiguous — or, worse, a lone hit in the *other* file reads as the
//! answer. Python says which class a name is: the file's own `class` statement
//! wins, and otherwise the `import` that bound the name. This module asks that
//! question first and only then reads the flat tables, restricted to the file
//! the name was proven to live in.

use super::*;

/// What a Python call on a class resolves to: the declaring file, the method's
/// identity, and the class that declares it (which is a base class when the
/// method is inherited).
pub(super) struct PythonMethod {
    pub(super) file: String,
    pub(super) symbol: String,
    pub(super) declaring_type: String,
}

/// A class in a Python MRO: one this corpus declares, or a base that is not
/// in it (external, builtin, or an import that names no class here) and so may
/// define any method.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum MroNode {
    Class(String, String),
    Opaque(String),
}

/// What one Python import statement binds a local name to. A file's imports
/// bind a name once only when they agree on one of these.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum PythonImportBind {
    /// `import a.b.c`: the package `a` (payload `a`), whatever else is imported
    /// under it. `import a.x` and `import a.y` bind the same package.
    Package(String),
    /// `import a.b as m`: the module `a.b`.
    Module(String),
    /// `from a import n [as m]`: the member spelled `a.n`, a submodule or a
    /// name the module declares.
    Member(String),
    /// `from a import *` written after this name was imported: the star may
    /// have rebound it, so the name is bound to more than one thing and no
    /// reading of it is trusted. The payload is the star's module.
    Star(String),
}

impl PythonImportBind {
    /// The dotted module path a member access on the bound name starts from.
    fn module_spec(&self) -> String {
        match self {
            Self::Package(spec) | Self::Module(spec) | Self::Member(spec) | Self::Star(spec) => {
                spec.clone()
            }
        }
    }
}

/// Why the file-scoped walk gave no linearization.
pub(super) enum MroStop {
    /// A base this file cannot bind or prove external: ask the flat table.
    NotScoped,
    /// A cycle, an inconsistent hierarchy or a bound hit: answer nothing.
    Abstain,
}

pub(super) enum MroAnswer {
    Found(PythonMethod),
    Abstain,
    NotScoped,
}

impl Resolver {
    /// Deepest chain of bases the C3 walk follows.
    const PYTHON_MRO_MAX_DEPTH: usize = 64;
    /// Most classes the C3 walk may linearize for one question.
    const PYTHON_MRO_MAX_NODES: usize = 512;

    /// The class `written` denotes in `file`, as `(declaring file, class
    /// identity)`: declared at file level here, or bound by an `import` to a
    /// class declared elsewhere. `None` when the name is neither, or is
    /// declared twice in the file.
    pub(super) fn python_class_named(&self, file: &str, written: &str) -> Option<(String, String)> {
        // `m.A`, written through a module the file imports: the same reading a
        // dotted base gets, and none when the root is not bound exactly once.
        if let Some((root, member)) = written.split_once('.') {
            let binds = self.python_effective_binds(file, root)?;
            return self.python_dotted_class(file, root, member, &binds);
        }
        let is_class = |path: &str, identity: &str| {
            self.symbol_index
                .get(identity.rsplit("::").next().unwrap_or(identity))
                .into_iter()
                .flatten()
                .any(|(candidate, kind, family, candidate_identity)| {
                    candidate == path
                        && **candidate_identity == *identity
                        && *kind == SymbolKind::Class
                        && *family == LangFamily::Python
                })
        };
        let local: Vec<&Arc<str>> = self
            .symbol_index
            .get(written)
            .into_iter()
            .flatten()
            .filter(|(path, kind, family, identity)| {
                path == file
                    && *kind == SymbolKind::Class
                    && *family == LangFamily::Python
                    && self.declared_at_file_level(path, identity)
            })
            .map(|(_, _, _, identity)| identity)
            .collect();
        match local.as_slice() {
            [only] => return Some((file.to_string(), only.to_string())),
            [] => {}
            _ => return None,
        }
        // The file also declares the name as something that is not a class
        // (`if TYPE_CHECKING: from a import Base` / `else: Base = object`): the
        // name is two things, and neither reading is the class statement's.
        if self.python_declares_at_file_level(file, written) {
            return None;
        }
        let (target_file, target_symbol) = self.import_bindings.get(file)?.get(written)?;
        let (class_file, identity) = self.lookup_in_package(target_file, target_symbol)?;
        is_class(&class_file, &identity).then_some((class_file, identity))
    }

    /// The bare type name `type_methods` and `supertypes` key a class by.
    fn python_type_key(identity: &str) -> &str {
        identity.rsplit("::").next().unwrap_or(identity)
    }

    /// The method `method` on the class `written` names in `file`: the class's
    /// own declaration, else the nearest declared base's. `None` — with no
    /// fallback to a same-named class elsewhere — when the class is known and
    /// the method is not found or not unique; `Err(())` when `written` is no
    /// class this file can name, so the caller may read the flat tables.
    pub(super) fn python_method_on(
        &self,
        file: &str,
        written: &str,
        method: &str,
    ) -> Result<Option<PythonMethod>, ()> {
        let (class_file, identity) = self.python_class_named(file, written).ok_or(())?;
        let type_key = Self::python_type_key(&identity);
        let own = self.python_own_methods(&class_file, type_key, method);
        match own.as_slice() {
            [(path, symbol)] => {
                return Ok(Some(PythonMethod {
                    file: path.clone(),
                    symbol: symbol.clone(),
                    declaring_type: type_key.to_string(),
                }));
            }
            [] => {}
            _ => return Ok(None),
        }
        Ok(self.python_inherited_from(&class_file, type_key, method))
    }

    /// The declarations of `method` that the class `type_key`, declared in
    /// `class_file`, writes on itself.
    fn python_own_methods(
        &self,
        class_file: &str,
        type_key: &str,
        method: &str,
    ) -> Vec<&(String, String)> {
        self.type_methods
            .get(&(LangFamily::Python, type_key.to_string(), method.to_string()))
            .into_iter()
            .flatten()
            .filter(|(path, _)| path == class_file)
            .collect()
    }

    /// `method` as the bases of the class `type_key` (declared in `class_file`)
    /// provide it: the file-scoped C3 walk first, the flat table only when that
    /// walk cannot speak for the class.
    ///
    /// The flat read refuses any class name two files declare, so a collision
    /// in the child's or a base's name used to lose the edge. The scoped walk
    /// asks Python's own question instead — which class does this file's
    /// `class` statement or `import` bind the base name to — and orders the
    /// answer the way Python does. It speaks, in either direction, for every
    /// base it can bind or prove external; it hands the question to the flat
    /// table only for a base it can neither bind nor prove external (a star
    /// import, a name declared twice in its file), where that table still
    /// abstains on a collision.
    pub(super) fn python_inherited_from(
        &self,
        class_file: &str,
        type_key: &str,
        method: &str,
    ) -> Option<PythonMethod> {
        match self.python_mro_method(class_file, type_key, method) {
            MroAnswer::Found(found) => Some(found),
            MroAnswer::Abstain => None,
            // The flat table keeps no position and no unnamed bases, so it
            // cannot speak for a class that writes one.
            MroAnswer::NotScoped
                if self
                    .python_bases
                    .get(&(class_file.to_string(), type_key.to_string()))
                    .is_some_and(|bases| bases.iter().any(|base| base == OPAQUE_BASE)) =>
            {
                None
            }
            MroAnswer::NotScoped => self.python_inherited_method(type_key, method),
        }
    }

    /// Whether `identity`, declared in `path`, is a Python class.
    fn python_is_class(&self, path: &str, identity: &str) -> bool {
        self.symbol_index
            .get(identity.rsplit("::").next().unwrap_or(identity))
            .into_iter()
            .flatten()
            .any(|(candidate, kind, family, candidate_identity)| {
                candidate == path
                    && **candidate_identity == *identity
                    && *kind == SymbolKind::Class
                    && *family == LangFamily::Python
            })
    }

    /// Whether `file` binds `name` with an `import` of any kind: one that
    /// resolved to an indexed module, one whose module is outside the corpus,
    /// or a relative one that names no indexed file. A star import binds no
    /// particular name and does not count.
    fn python_name_is_imported(&self, file: &str, name: &str) -> bool {
        self.import_bindings
            .get(file)
            .is_some_and(|bound| bound.contains_key(name))
            || self
                .external_imports
                .get(file)
                .is_some_and(|bound| bound.contains_key(name))
            || self
                .unindexed_local_imports
                .get(file)
                .is_some_and(|bound| bound.contains_key(name))
    }

    /// The MRO node a base written in `file` as `written` denotes.
    ///
    /// A class this file names (its own `class`, or the import that bound it)
    /// is [`MroNode::Class`]. A base that is provably not in the corpus — bound
    /// by an import to a module outside it, reached through a dotted name whose
    /// root is such an import, or a name nothing declares and nothing imports
    /// (`dict`, `Exception`) — is [`MroNode::Opaque`]: it may define any
    /// method, so it blocks a lookup that would pass through it. A name the
    /// corpus declares that this file cannot bind to one declaration is
    /// [`MroStop::NotScoped`].
    fn python_mro_node(&self, file: &str, written: &str) -> Result<MroNode, MroStop> {
        // A standard-library member the file's import names: one class whatever
        // the spelling (`Counter`, `typing.Counter`, `t.Counter`), so it is one
        // node, and one whose methods the standard library can answer.
        if let Some(member) = self.python_stdlib_member(file, written) {
            return Ok(MroNode::Opaque(member));
        }
        if let Some((root, member)) = written.split_once('.') {
            let Some(binds) = self.python_effective_binds(file, root) else {
                return if self.python_name_is_imported(file, root) {
                    Ok(MroNode::Opaque(written.to_string()))
                } else if self.declares_type(
                    LangFamily::Python,
                    member.rsplit('.').next().unwrap_or(member),
                ) {
                    Err(MroStop::NotScoped)
                } else {
                    Ok(MroNode::Opaque(written.to_string()))
                };
            };
            return Ok(self
                .python_dotted_class(file, root, member, &binds)
                .map_or_else(
                    || MroNode::Opaque(written.to_string()),
                    |(path, identity)| MroNode::Class(path, identity),
                ));
        }
        // A name the file binds to more than one thing (`if TYPE_CHECKING:
        // from a import Base` / `else: from b import Base`, a class in one
        // branch and a function in the other) is whichever one ran: no class
        // statement can be read as naming either, so it is an unknown base.
        if self.python_name_is_ambiguous(file, written) {
            return Ok(MroNode::Opaque(written.to_string()));
        }
        if let Some((path, identity)) = self.python_class_named(file, written) {
            return Ok(MroNode::Class(path, identity));
        }
        if self.python_name_is_imported(file, written) {
            return Ok(MroNode::Opaque(written.to_string()));
        }
        if self.declares_type(LangFamily::Python, written) {
            return Err(MroStop::NotScoped);
        }
        Ok(MroNode::Opaque(written.to_string()))
    }

    /// The class a dotted base `root.member` names, where `root` is a name the
    /// file's imports bind (`binds` is everything they bind it to).
    ///
    /// Python's own reading, and nothing else: the root must be bound exactly
    /// once in the file (a second `import ... as o`, or a module-level `o = ..`,
    /// means the class statement may see either, and the answer is none); each
    /// segment but the last extends the module path (`a.b.C` asks module
    /// `a.b`); and the module is the file the ordinary import resolution
    /// names. The last segment is a class that module declares or re-exports
    /// (a package `__init__` that lifts it out of a submodule). `None` for
    /// anything else, which the caller reads as an opaque base — never the
    /// flat table, because the root is an import and the file said where it
    /// comes from.
    fn python_dotted_class(
        &self,
        file: &str,
        root: &str,
        member: &str,
        binds: &BTreeSet<PythonImportBind>,
    ) -> Option<(String, String)> {
        let mut only = binds.iter();
        let (Some(bind), None) = (only.next(), only.next()) else {
            return None;
        };
        if self.python_declares_at_file_level(file, root) {
            return None;
        }
        let (middle, class_name) = match member.rsplit_once('.') {
            Some((middle, class_name)) => (Some(middle), class_name),
            None => (None, member),
        };
        let module_file = match middle {
            // `import pkg.mod as m; class C(m.Base)`, `import one; one.Base`:
            // the binding the import walk already resolved, which follows the
            // file's `sys.path` evidence.
            None => self
                .import_bindings
                .get(file)
                .and_then(|bound| bound.get(root))
                .filter(|_| !self.is_value_import(file, root))
                .map(|(module_file, _)| module_file.clone())
                .or_else(|| self.resolve_import_path(file, "python", &bind.module_spec()))?,
            Some(middle) => self.resolve_import_path(
                file,
                "python",
                &format!("{}.{middle}", bind.module_spec()),
            )?,
        };
        let (class_file, identity) = {
            let (declaring_file, declared) = self.declaring_site(&module_file, class_name);
            self.lookup_in_package(&declaring_file, &declared)?
        };
        self.python_is_class(&class_file, &identity)
            .then_some((class_file, identity))
    }

    /// Whether `file` declares `name` at file level under any kind: a class, a
    /// function or a module-level assignment all rebind an imported name.
    fn python_declares_at_file_level(&self, file: &str, name: &str) -> bool {
        self.symbol_index
            .get(name)
            .into_iter()
            .flatten()
            .any(|(path, _, family, identity)| {
                path == file
                    && *family == LangFamily::Python
                    && self.declared_at_file_level(path, identity)
            })
    }

    /// Whether `written`, a base in `file`, is a structural marker with no
    /// methods of its own that can precede a real base in the MRO.
    ///
    /// Python's typing semantics fix exactly three such names: `Generic` and
    /// `Protocol` (typing, typing_extensions) only parameterise or tag the
    /// class, and `abc.ABC` only supplies the `ABCMeta` metaclass. Every other
    /// name from those modules is a real class or an alias of one
    /// (`typing.Counter` is `collections.Counter`, `typing.Mapping` is
    /// `collections.abc.Mapping`, `abc.ABCMeta` is a metaclass), so it stays in
    /// the MRO as an opaque base ([`Self::python_stdlib_member`]).
    fn python_base_is_structural(&self, file: &str, written: &str) -> bool {
        self.python_stdlib_member(file, written)
            .is_some_and(|member| {
                matches!(
                    member.as_str(),
                    "typing.Generic"
                        | "typing.Protocol"
                        | "typing_extensions.Generic"
                        | "typing_extensions.Protocol"
                        | "abc.ABC"
                )
            })
    }

    /// The `module.Member` a base `written` in `file` names when the file's one
    /// import binds it to a member of `typing`, `typing_extensions` or `abc`
    /// (`Generic`, `typing.Generic`, `import typing as t; t.Generic`). Decided
    /// by the module the import names, never by the class name, and only when
    /// that module is not one this corpus declares.
    fn python_stdlib_member(&self, file: &str, written: &str) -> Option<String> {
        let (root, member) = match written.split_once('.') {
            Some((root, member)) => (root, Some(member)),
            None => (written, None),
        };
        // A later `from m import *` is not read as rebinding the marker: the
        // member is identified by the module the import resolved, and the one
        // import of it is the only binding the file's own imports make.
        let binds: BTreeSet<&PythonImportBind> = self
            .python_import_binds
            .get(file)
            .and_then(|bound| bound.get(root))?
            .iter()
            .filter(|bind| !matches!(bind, PythonImportBind::Star(_)))
            .collect();
        let mut only = binds.into_iter();
        let (Some(bind), None) = (only.next(), only.next()) else {
            return None;
        };
        if self.python_declares_at_file_level(file, root) {
            return None;
        }
        let qualified = match (bind, member) {
            // `from typing import Generic`: the member spelled `typing.Generic`.
            (PythonImportBind::Member(spec), None) => spec.clone(),
            (PythonImportBind::Member(_) | PythonImportBind::Star(_), _) => return None,
            // `typing.Generic`, `import typing as t; t.Generic`.
            (_, Some(member)) => format!("{}.{member}", bind.module_spec()),
            (_, None) => return None,
        };
        let (module, _) = qualified.rsplit_once('.')?;
        (matches!(module, "typing" | "typing_extensions" | "abc")
            && self.resolve_import_path(file, "python", module).is_none())
        .then_some(qualified)
    }

    /// What `file`'s imports bind the local `name` to, minus every `from m
    /// import *` that cannot rebind it: a star import written after the name's
    /// import rebinds it only when `m` could define it
    /// ([`Self::python_star_may_define`]). `None` when no import binds it.
    fn python_effective_binds(&self, file: &str, name: &str) -> Option<BTreeSet<PythonImportBind>> {
        let bound = self.python_import_binds.get(file)?.get(name)?;
        Some(
            bound
                .iter()
                .filter(|bind| match bind {
                    PythonImportBind::Star(spec) => self.python_star_may_define(file, spec, name),
                    _ => true,
                })
                .cloned()
                .collect(),
        )
    }

    /// Whether `from spec import *`, written in `file`, can bind `name`.
    ///
    /// A module outside the corpus, or one that does not resolve, can define
    /// anything. An in-repo module is read by what `import *` takes from it: the
    /// names its `__all__` lists when it has one, otherwise every public name it
    /// declares, imports or file-scope binds, and anything at all when it has a
    /// star import of its own.
    fn python_star_may_define(&self, file: &str, spec: &str, name: &str) -> bool {
        let Some(module) = self.resolve_import_path(file, "python", spec) else {
            return true;
        };
        let all_key = format!("{module}::__all__");
        let lowest = (module.clone(), all_key.clone(), String::new());
        let has_all = self
            .scope_locals
            .range(lowest..)
            .next()
            .is_some_and(|(path, scope, _)| *path == module && *scope == all_key);
        if has_all {
            return self
                .scope_locals
                .contains(&(module, all_key, name.to_string()));
        }
        if name.starts_with('_') {
            return false;
        }
        self.python_import_binds
            .get(&module)
            .is_some_and(|bound| bound.contains_key(name) || bound.contains_key("*"))
            || self.python_declares_at_file_level(&module, name)
            || self.file_scope_declares(&module, name)
    }

    /// Whether `file` binds `name` more than once at module scope — two
    /// imports of different targets, an import and a declaration, a class and a
    /// function — or binds it in a way the declaration tables do not carry (a
    /// `for` target, a `del`, a `global` write). Which binding a class statement
    /// reads is then a matter of which branch ran.
    fn python_name_is_ambiguous(&self, file: &str, name: &str) -> bool {
        let imports = self
            .python_effective_binds(file, name)
            .map_or(0, |bound| bound.len());
        let declarations = self
            .symbol_index
            .get(name)
            .into_iter()
            .flatten()
            .filter(|(path, _, family, identity)| {
                path == file
                    && *family == LangFamily::Python
                    && self.declared_at_file_level(path, identity)
            })
            .count();
        imports + declarations > 1 || self.file_scope_declares(file, name)
    }

    /// The C3 linearization of `node` (the node first), memoised.
    ///
    /// Bounded by [`Self::PYTHON_MRO_MAX_DEPTH`] and by the number of nodes
    /// the walk may create; an inheritance cycle, an inconsistent hierarchy
    /// (where C3 has no answer — Python raises `TypeError` there) and a
    /// bound hit are all [`MroStop::Abstain`].
    fn python_linearize(
        &self,
        node: &MroNode,
        depth: usize,
        memo: &mut BTreeMap<MroNode, Vec<MroNode>>,
        in_progress: &mut BTreeSet<MroNode>,
    ) -> Result<Vec<MroNode>, MroStop> {
        if let Some(done) = memo.get(node) {
            return Ok(done.clone());
        }
        let MroNode::Class(file, identity) = node else {
            return Ok(vec![node.clone()]);
        };
        if depth > Self::PYTHON_MRO_MAX_DEPTH
            || memo.len() > Self::PYTHON_MRO_MAX_NODES
            || !in_progress.insert(node.clone())
        {
            return Err(MroStop::Abstain);
        }
        let key = Self::python_type_key(identity).to_string();
        let written: &[String] = match self.python_bases.get(&(file.clone(), key)) {
            Some(bases) => bases,
            // The root of the walk has no recorded heritage: nothing here says
            // whether that is a class with no bases or a key this table never
            // learned, so the flat table is asked instead of answering "none".
            None if depth == 0 => return Err(MroStop::NotScoped),
            None => &[],
        };
        let mut bases: Vec<MroNode> = Vec::new();
        for (position, base) in written.iter().enumerate() {
            if base == OPAQUE_BASE {
                // A base the class statement writes but no name denotes: an
                // unknown class in this position, distinct from every other.
                bases.push(MroNode::Opaque(format!("{OPAQUE_BASE}@{position}")));
            } else if !self.python_base_is_structural(file, base) {
                bases.push(self.python_mro_node(file, base)?);
            }
        }
        let mut sequences: Vec<Vec<MroNode>> = Vec::new();
        for base in &bases {
            sequences.push(self.python_linearize(base, depth + 1, memo, in_progress)?);
        }
        sequences.push(bases);
        let mut linear = vec![node.clone()];
        loop {
            sequences.retain(|sequence| !sequence.is_empty());
            if sequences.is_empty() {
                break;
            }
            // The first head that appears in no sequence's tail.
            let head = sequences
                .iter()
                .map(|sequence| &sequence[0])
                .find(|head| sequences.iter().all(|other| !other[1..].contains(head)))
                .cloned()
                .ok_or(MroStop::Abstain)?;
            for sequence in &mut sequences {
                if sequence[0] == head {
                    sequence.remove(0);
                }
            }
            linear.push(head);
        }
        in_progress.remove(node);
        memo.insert(node.clone(), linear.clone());
        Ok(linear)
    }

    /// The one method `method` that the first class in the MRO of `type_key`
    /// (declared in `class_file`) declares, after the class itself.
    ///
    /// An [`MroNode::Opaque`] class ahead of that class abstains: the external
    /// or builtin base comes first in Python's order and might define the
    /// method itself. A class that declares the name twice abstains too.
    pub(super) fn python_mro_method(
        &self,
        class_file: &str,
        type_key: &str,
        method: &str,
    ) -> MroAnswer {
        let root = MroNode::Class(class_file.to_string(), type_key.to_string());
        let mut memo = BTreeMap::new();
        let mut in_progress = BTreeSet::new();
        let linear = match self.python_linearize(&root, 0, &mut memo, &mut in_progress) {
            Ok(linear) => linear,
            Err(MroStop::NotScoped) => return MroAnswer::NotScoped,
            Err(MroStop::Abstain) => return MroAnswer::Abstain,
        };
        for node in linear.iter().skip(1) {
            match node {
                MroNode::Opaque(_) => return MroAnswer::Abstain,
                MroNode::Class(file, identity) => {
                    let key = Self::python_type_key(identity);
                    match self.python_own_methods(file, key, method).as_slice() {
                        [] => {}
                        [(path, symbol)] => {
                            return MroAnswer::Found(PythonMethod {
                                file: path.clone(),
                                symbol: symbol.clone(),
                                declaring_type: key.to_string(),
                            });
                        }
                        _ => return MroAnswer::Abstain,
                    }
                }
            }
        }
        MroAnswer::Abstain
    }

    /// `method` as the declared bases of `type_key` provide it. Needs the class
    /// name to be unique in the corpus, because `supertypes` is keyed by it.
    pub(super) fn python_inherited_method(
        &self,
        type_key: &str,
        method: &str,
    ) -> Option<PythonMethod> {
        if !self.type_name_is_identifiable(LangFamily::Python, type_key) {
            return None;
        }
        let bases: Vec<String> = self
            .supertypes
            .get(&(LangFamily::Python, type_key.to_string()))?
            .iter()
            .cloned()
            .collect();
        let (file, symbol, declaring_type) =
            self.supertype_method(LangFamily::Python, bases, method)?;
        Some(PythonMethod {
            file,
            symbol,
            declaring_type,
        })
    }

    /// The method a `super().method()` call reaches from a call attributed to
    /// `caller`: the enclosing class's declared bases, never the enclosing
    /// class itself.
    pub(super) fn python_super_method(
        &self,
        file: &str,
        caller: &str,
        method: &str,
    ) -> Option<PythonMethod> {
        let enclosing = self.self_type_at(file, LangFamily::Python, caller)?;
        self.python_inherited_from(file, &enclosing, method)
    }

    /// The class a `cls(...)` call inside a method of it constructs.
    pub(super) fn python_cls_target(&self, file: &str, caller: &str) -> Option<(String, String)> {
        let enclosing = self.self_type_at(file, LangFamily::Python, caller)?;
        self.python_class_named(file, &enclosing)
    }

    /// The class name a Python scope symbol is, as the enclosing-type tables
    /// spell it (the tail of the scope after `::`), when it is a class.
    ///
    /// A class written inside a function carries its parents' names in its own
    /// identity (`H.__init__.Inner`) while its members' parent says `Inner`, so
    /// the scope's spelling may be the end of the identity.
    pub(super) fn python_class_scope(&self, file: &str, scope: &str) -> Option<String> {
        let tail = scope.rsplit("::").next().filter(|tail| !tail.is_empty())?;
        let bare = tail.rsplit('.').next().unwrap_or(tail);
        self.symbol_index
            .get(bare)
            .into_iter()
            .flatten()
            .any(|(path, kind, family, identity)| {
                let declared = identity.rsplit("::").next().unwrap_or(identity);
                path == file
                    && *kind == SymbolKind::Class
                    && *family == LangFamily::Python
                    && (declared == tail || declared.ends_with(&format!(".{tail}")))
            })
            .then(|| tail.to_string())
    }

    /// Whether the Python name `root`, read in `scope` of `file`, is a binding
    /// of something other than the one thing the file's imports and
    /// declarations make it: bound more than once at module scope (an import
    /// and a later assignment, two imports), or by a function that encloses
    /// `scope`, which a closure's free variable reads first.
    pub(super) fn python_name_is_rebound(&self, file: &str, scope: &str, root: &str) -> bool {
        // Module scope: bound more than once, or by a `for`/`with`/`except`
        // target, walrus, `match` capture, `del` or `global` write, which the
        // import and declaration tables do not carry (the extractor files each
        // under the file's own name).
        self.python_name_is_ambiguous(file, root)
            || self.python_scope_binds_name(file, scope, root, 0)
    }

    /// Whether `root` is bound by a function that encloses the class named
    /// `tail` (or by one that encloses `scope`), walking out from `scope`.
    ///
    /// A class written inside a function is recorded with the file as its
    /// parent, and its identity (`build.H`) is the same string as a class `H`
    /// nested in a module-level class `build`. What encloses a class is
    /// therefore read from the symbols' own spans ([`python_class_enclosers`]),
    /// by node kind: a function encloses it or a class does, never "the one
    /// named `build`". A class name that several enclosers share is read as
    /// each of them, and any function among them that binds `root` answers
    /// yes: a conservative abstention, never a binding of the wrong scope.
    fn python_scope_binds_name(&self, file: &str, scope: &str, root: &str, depth: usize) -> bool {
        let mut current = scope.to_string();
        let mut callable = depth > 0;
        for _ in 0..Self::PYTHON_MRO_MAX_DEPTH {
            let class_tail = if callable {
                None
            } else {
                self.python_class_scope(file, &current)
            };
            // A class body's names are not in scope inside its methods.
            if class_tail.is_none() && self.scope_declares_local(file, &current, root) {
                return true;
            }
            callable = false;
            let enclosers = class_tail.as_deref().and_then(|tail| {
                let bare = tail.rsplit('.').next().unwrap_or(tail);
                self.python_class_enclosers
                    .get(&(file.to_string(), bare.to_string()))
            });
            if let Some(enclosers) = enclosers {
                let mut classes = Vec::new();
                for (enclosing, is_callable) in enclosers {
                    if *is_callable {
                        if self.python_scope_binds_name(file, enclosing, root, depth + 1) {
                            return true;
                        }
                    } else {
                        classes.push(enclosing.clone());
                    }
                }
                match classes.as_slice() {
                    [] => return false,
                    [only] => {
                        current = only.clone();
                        continue;
                    }
                    _ => return false,
                }
            }
            let Some(parent) = self
                .symbol_parents
                .get(&(file.to_string(), current.clone()))
                .filter(|parent| parent.as_str() != file && **parent != current)
            else {
                return false;
            };
            current = parent.clone();
        }
        true
    }
}

/// For each class a Python file declares, what it is written inside: the
/// nearest enclosing function, method or class among the file's symbols, found
/// by span containment. Keyed by `(file, class name)`; a name written at
/// several places lists each. The enclosing symbol is `(identity, is a
/// callable)`; a class at file level has none.
pub(super) fn python_class_enclosers(ext: &Extraction) -> Vec<((String, String), (String, bool))> {
    let mut containers: Vec<&ExtractedSymbol> = ext
        .symbols
        .iter()
        .filter(|sym| {
            matches!(
                sym.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Class
            )
        })
        .collect();
    containers.sort_by_key(|sym| (sym.span.start_byte, std::cmp::Reverse(sym.span.end_byte)));
    let mut open: Vec<&ExtractedSymbol> = Vec::new();
    let mut out = Vec::new();
    for sym in containers {
        while open
            .last()
            .is_some_and(|top| top.span.end_byte <= sym.span.start_byte)
        {
            open.pop();
        }
        if sym.kind == SymbolKind::Class {
            if let Some(top) = open.last() {
                out.push((
                    (ext.file_path.clone(), sym.name.clone()),
                    (top.qualified_name.clone(), top.kind != SymbolKind::Class),
                ));
            }
        }
        open.push(sym);
    }
    out
}

/// What a Python file's imports bind each local name to: `import a.b` binds
/// the package `a`, `import a.b as m` the module `a.b`, and `from a import n
/// as m` the member `n` of `a`. Every import counts, at any depth: a name a
/// function imports again is a second binding the class statement may or may
/// not see.
pub(super) fn python_import_binds(
    ext: &Extraction,
) -> BTreeMap<String, BTreeSet<PythonImportBind>> {
    let mut binds: BTreeMap<String, BTreeSet<PythonImportBind>> = BTreeMap::new();
    // Where each name is first imported, and each `from m import *`: a star
    // written after a name's import may rebind it.
    let mut first_import: BTreeMap<String, usize> = BTreeMap::new();
    let mut stars: Vec<(usize, &str)> = Vec::new();
    for imp in &ext.imports {
        if imp.path_load.is_some() {
            continue;
        }
        let specifier = imp.module_specifier.as_str();
        if imp.imported_names.is_empty() {
            let (local, bind) = match imp.alias.as_deref() {
                Some("*") => {
                    stars.push((imp.span.start_byte, specifier));
                    continue;
                }
                Some(".") => continue,
                Some(alias) => (alias, PythonImportBind::Module(specifier.to_string())),
                None => {
                    let root = specifier.split('.').next().unwrap_or(specifier);
                    (root, PythonImportBind::Package(root.to_string()))
                }
            };
            if !local.is_empty() {
                first_import
                    .entry(local.to_string())
                    .and_modify(|start| *start = (*start).min(imp.span.start_byte))
                    .or_insert(imp.span.start_byte);
                binds.entry(local.to_string()).or_default().insert(bind);
            }
            continue;
        }
        for (index, name) in imp.imported_names.iter().enumerate() {
            if name.is_empty() || name == "*" {
                continue;
            }
            let local = imp
                .local_names
                .get(index)
                .map(String::as_str)
                .filter(|local| !local.is_empty())
                .or_else(|| {
                    (imp.imported_names.len() == 1)
                        .then_some(imp.alias.as_deref())
                        .flatten()
                })
                .unwrap_or(name.as_str());
            let member = if specifier.chars().all(|c| c == '.') {
                format!("{specifier}{name}")
            } else {
                format!("{specifier}.{name}")
            };
            first_import
                .entry(local.to_string())
                .and_modify(|start| *start = (*start).min(imp.span.start_byte))
                .or_insert(imp.span.start_byte);
            binds
                .entry(local.to_string())
                .or_default()
                .insert(PythonImportBind::Member(member));
        }
    }
    for (star_start, module) in stars {
        // The file's own stars, under a key no identifier can spell: a module
        // with one may define any name ([`Resolver::python_star_may_define`]).
        binds
            .entry("*".to_string())
            .or_default()
            .insert(PythonImportBind::Star(module.to_string()));
        for (local, import_start) in &first_import {
            if *import_start < star_start {
                if let Some(bound) = binds.get_mut(local) {
                    bound.insert(PythonImportBind::Star(module.to_string()));
                }
            }
        }
    }
    binds
}
