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

/// What binds a name in the nearest function that binds it; see
/// [`Resolver::python_scope_lookup`].
pub(super) enum ScopeHit<'a> {
    /// An import written in the function; `None` when it names no indexed
    /// file, or the function imports the name from two places.
    Import(Option<&'a (String, String)>),
    /// Any other binding: a parameter, an assignment, a `for` target.
    Local,
}

/// Why the file-scoped walk gave no linearization.
pub(super) enum MroStop {
    /// A base this file cannot bind or prove external: ask the flat table.
    NotScoped,
    /// A cycle or an inconsistent hierarchy: answer nothing.
    Abstain,
    /// The walk reached one of its bounds. The reason names the bound, so the
    /// site's ledger row says why nothing was answered.
    Capped(String),
}

pub(super) enum MroAnswer {
    Found(PythonMethod),
    Abstain,
    NotScoped,
    /// The walk reached a bound; see [`MroStop::Capped`].
    Capped(String),
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
    ///
    /// `scope` is the callable the name is written in (`Some(file)` for the
    /// module): a function that binds the name itself is not the module's
    /// reading of it ([`Self::python_import_binding`]).
    pub(super) fn python_class_named(
        &self,
        file: &str,
        scope: Option<&str>,
        written: &str,
    ) -> Option<(String, String)> {
        // `m.A`, written through a module the file imports: the same reading a
        // dotted base gets, and none when the root is not bound exactly once.
        if let Some((root, member)) = written.split_once('.') {
            if scope
                .filter(|scope| *scope != file)
                .is_some_and(|scope| self.python_scope_lookup(file, scope, root, 0).is_some())
            {
                return None;
            }
            let binds = self.python_effective_binds(file, root)?;
            return self.python_dotted_class(file, scope, root, member, &binds);
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
        let (target_file, target_symbol) = self.python_import_binding(file, scope, written)?;
        let (class_file, identity) = self.lookup_in_package(target_file, target_symbol)?;
        is_class(&class_file, &identity).then_some((class_file, identity))
    }

    /// The bare class name a type written as `written` in `declaring_file`'s
    /// module scope denotes, so a name another file reads it by is the class
    /// that file meant: `-> L` where the file wrote `from real import Log as L`
    /// is `Log`, never an unrelated corpus-unique `L`.
    ///
    /// A name the file binds to a class is that class. A name an import binds
    /// that the walk cannot follow (an external module, a star import, a
    /// conditional alternate) names nothing here — no fallback to a bare name
    /// the corpus happens to declare once. Only a name no import of the file
    /// claims is read as written.
    pub(super) fn python_annotation_class(
        &self,
        declaring_file: &str,
        written: &str,
    ) -> Option<String> {
        if let Some((_, identity)) =
            self.python_class_named(declaring_file, Some(declaring_file), written)
        {
            return Some(Self::python_type_key(&identity).to_string());
        }
        let root = written.split('.').next().unwrap_or(written);
        let binds = self.python_import_binds.get(declaring_file);
        let import_bound =
            binds.is_some_and(|binds| binds.contains_key(root) || binds.contains_key("*"));
        (!import_bound).then(|| written.to_string())
    }

    /// Whether exactly one thing provides the bare name `name` to `file` at
    /// module scope: not two imports of it (a `try`/`except ImportError`
    /// alternate), and not two `from m import *` that may both define it —
    /// the last star wins in Python, and which one ran is not a static fact.
    pub(super) fn python_bare_name_has_one_provider(&self, file: &str, name: &str) -> bool {
        let explicit = self
            .python_effective_binds(file, name)
            .map_or(0, |bound| bound.len());
        if explicit > 1 {
            return false;
        }
        if explicit == 1 {
            return true;
        }
        let stars = self
            .python_import_binds
            .get(file)
            .and_then(|binds| binds.get("*"));
        stars.is_none_or(|stars| {
            stars
                .iter()
                .filter(|star| match star {
                    PythonImportBind::Star(spec) => self.python_star_may_define(file, spec, name),
                    _ => false,
                })
                .count()
                <= 1
        })
    }

    /// Whether a function or class named `name` is declared directly inside
    /// the callable `scope`, which shadows an import or module-level binding of
    /// that name for every read written in it.
    fn python_nested_declares(&self, file: &str, scope: &str, name: &str) -> bool {
        self.symbol_index
            .get(name)
            .into_iter()
            .flatten()
            .any(|(path, kind, family, identity)| {
                path == file
                    && *family == LangFamily::Python
                    && matches!(
                        kind,
                        SymbolKind::Function | SymbolKind::Method | SymbolKind::Class
                    )
                    && self
                        .symbol_parents
                        .get(&(file.to_string(), identity.to_string()))
                        .is_some_and(|parent| parent == scope)
            })
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
        scope: Option<&str>,
        written: &str,
        method: &str,
    ) -> Result<Option<PythonMethod>, ()> {
        let (class_file, identity) = self.python_class_named(file, scope, written).ok_or(())?;
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
        match self.python_mro_method(class_file, type_key, method, false) {
            MroAnswer::Found(found) => Some(found),
            MroAnswer::Abstain | MroAnswer::Capped(_) => None,
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
            // The flat walk is breadth-first, which is C3 only along a chain.
            MroAnswer::NotScoped if !self.python_flat_heritage_is_linear(class_file, type_key) => {
                None
            }
            MroAnswer::NotScoped => self.python_inherited_method(type_key, method),
        }
    }

    /// Whether the heritage of the class `type_key`, as the flat tables record
    /// it, is a single chain: no class on it has more than one base (an
    /// opaque base counts as one). On a chain the breadth-first walk and C3
    /// visit classes in the same order, so a class the file-scoped walk cannot
    /// speak for may be read from the flat table. A chain longer than
    /// [`Resolver::HERITAGE_WALK_MAX_DEPTH`] is not known to be one.
    pub(super) fn python_flat_heritage_is_linear(&self, file: &str, type_key: &str) -> bool {
        let mut current = type_key.to_string();
        for depth in 0..Self::HERITAGE_WALK_MAX_DEPTH {
            let flat = self
                .supertypes
                .get(&(LangFamily::Python, current.clone()))
                .map_or(0, |bases| bases.len());
            // An opaque base at any link of the chain is a second base there:
            // the root's own file for the root, and every file that declares a
            // class of that name for the links the flat table names by bare name.
            let opaque_in = |path: &str| {
                self.python_bases
                    .get(&(path.to_string(), current.clone()))
                    .map_or(0, |bases| {
                        bases.iter().filter(|base| *base == OPAQUE_BASE).count()
                    })
            };
            let opaque = if depth == 0 {
                opaque_in(file)
            } else {
                self.symbol_index
                    .get(current.rsplit('.').next().unwrap_or(&current))
                    .into_iter()
                    .flatten()
                    .filter(|(_, kind, family, _)| {
                        *kind == SymbolKind::Class && *family == LangFamily::Python
                    })
                    .map(|(path, _, _, _)| opaque_in(path))
                    .max()
                    .unwrap_or(0)
            };
            match flat + opaque {
                0 => return true,
                1 => {}
                _ => return false,
            }
            let Some(only) = self
                .supertypes
                .get(&(LangFamily::Python, current.clone()))
                .and_then(|bases| bases.iter().next())
            else {
                return true;
            };
            current = only.clone();
        }
        false
    }

    /// Whether the base `written` of the class `identity` is read in a scope
    /// that rebinds its first name: a function the class is written inside
    /// binds it (a parameter, a local, an import), or a class body it is
    /// written inside imports it. The file's module-scope reading of the name
    /// is then not what the class statement sees.
    fn python_base_is_rebound_around(&self, file: &str, identity: &str, written: &str) -> bool {
        let key = Self::python_type_key(identity);
        let bare = key.rsplit('.').next().unwrap_or(key);
        let Some(enclosers) = self
            .python_class_enclosers
            .get(&(file.to_string(), bare.to_string()))
        else {
            return false;
        };
        let root = written.split('.').next().unwrap_or(written);
        enclosers.iter().any(|(enclosing, is_callable)| {
            if *is_callable {
                self.python_scope_binds_name(file, enclosing, root, 1)
            } else {
                self.python_scoped_import_bindings
                    .get(&(file.to_string(), enclosing.clone()))
                    .is_some_and(|names| names.contains_key(root))
            }
        })
    }

    /// How many `class` statements `file` writes under the identity `identity`.
    fn python_class_declarations(&self, file: &str, identity: &str) -> usize {
        let key = Self::python_type_key(identity);
        let bare = key.rsplit('.').next().unwrap_or(key);
        self.symbol_index
            .get(bare)
            .into_iter()
            .flatten()
            .filter(|(path, kind, family, candidate_identity)| {
                path == file
                    && Self::python_type_key(candidate_identity) == key
                    && *kind == SymbolKind::Class
                    && *family == LangFamily::Python
            })
            .count()
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
                .python_dotted_class(file, Some(file), root, member, &binds)
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
        if let Some((path, identity)) = self.python_class_named(file, Some(file), written) {
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
        scope: Option<&str>,
        root: &str,
        member: &str,
        binds: &BTreeSet<PythonImportBind>,
    ) -> Option<(String, String)> {
        let (class_file, identity) =
            self.python_dotted_declaration(file, scope, root, member, binds)?;
        self.python_is_class(&class_file, &identity)
            .then_some((class_file, identity))
    }

    /// What `call`'s receiver is when it is the *result of a call*, as
    /// `Some(its type)` (or `Some(None)` when that type is not known), and `None`
    /// when the receiver is not a call result at all.
    ///
    /// The extractor reduces the receiver of `make(x).m()` to the callee text
    /// `make`, the same text `make.m()` records. What tells them apart is the
    /// inner call `make(x)`: it begins where the outer call begins, ends before
    /// it, and its own identity (`make`, or `mod.make` for a member) is the
    /// receiver text. `inner` holds the calls that begin at the outer call's
    /// start. The type is the callee's *written* return annotation, read by
    /// [`Self::factory_return_type`] — the same owner `x = make()` uses — so an
    /// unannotated, generic or optional return types nothing, and neither does
    /// a callee name the calling scope binds itself.
    pub(super) fn python_call_result_type(
        &self,
        file: &str,
        scope: Option<&str>,
        call: &ExtractedCall,
        receiver: &str,
        inner: &[&ExtractedCall],
    ) -> Option<Option<String>> {
        let is_the_receiver = inner.iter().any(|candidate| {
            !std::ptr::eq(*candidate, call)
                && candidate.caller_symbol == call.caller_symbol
                && candidate.span.end_byte < call.span.end_byte
                && match candidate.receiver_expr.as_deref() {
                    Some(qualifier) => receiver == format!("{qualifier}.{}", candidate.callee_name),
                    None => receiver == candidate.callee_name,
                }
        });
        is_the_receiver.then(|| self.factory_return_type(LangFamily::Python, file, scope, receiver))
    }

    /// The declaration `root.member` names, where `root` is a module the file
    /// imports and `member` is a dotted path through it ending in the declared
    /// name: `(declaring file, identity)`. The reading
    /// [`Self::python_dotted_class`] documents, without its class test, so a
    /// dotted base class and a dotted module-function call ask one owner.
    fn python_dotted_declaration(
        &self,
        file: &str,
        scope: Option<&str>,
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
                .python_import_binding(file, scope, root)
                .filter(|_| !self.is_value_import(file, root))
                .map(|(module_file, _)| module_file.clone())
                .or_else(|| self.resolve_import_path(file, "python", &bind.module_spec()))?,
            Some(middle) => self.resolve_import_path(
                file,
                "python",
                &format!("{}.{middle}", bind.module_spec()),
            )?,
        };
        let (declaring_file, declared) = self.declaring_site(&module_file, class_name);
        self.lookup_in_package(&declaring_file, &declared)
    }

    /// The declaration `module_path.member` names, where `module_path` is a
    /// dotted module path (`a.b.c`) the file reached with a module-level `import
    /// a.b.c` and nothing rebinds: `(declaring file, identity)`.
    ///
    /// `import a.b.c` binds only the package `a`, so `a.b.c.f()` is a call on a
    /// path the *import statement* made reachable. Hence three conditions beyond
    /// the dotted-declaration reading: this file has a module-level import of
    /// that path (or of a longer one under it), the root is bound by that
    /// package import alone, and the function the call sits in does not bind the
    /// root itself. A path no import here reaches names nothing.
    pub(super) fn python_module_path_member(
        &self,
        ext: &Extraction,
        containers: Option<&ScopeIndex<'_>>,
        scope: Option<&str>,
        module_path: &str,
        member: &str,
    ) -> Option<(String, String)> {
        let file = ext.file_path.as_str();
        let (root, middle) = module_path.split_once('.')?;
        if !module_path.split('.').all(Self::is_plain_ident) {
            return None;
        }
        let nested = format!("{module_path}.");
        let reached = ext.imports.iter().any(|import| {
            import.imported_names.is_empty()
                && import.alias.is_none()
                && (import.module_specifier == module_path
                    || import.module_specifier.starts_with(&nested))
                && containers.is_none_or(|containers| {
                    python_innermost_container(containers, &import.span).is_none()
                })
        });
        if !reached {
            return None;
        }
        if scope
            .filter(|scope| *scope != file)
            .is_some_and(|scope| self.python_scope_lookup(file, scope, root, 0).is_some())
        {
            return None;
        }
        let binds = self.python_effective_binds(file, root)?;
        if !matches!(
            binds.iter().collect::<Vec<_>>().as_slice(),
            [PythonImportBind::Package(package)] if package == root
        ) {
            return None;
        }
        // Any other binding of the root anywhere in the module — a `for`,
        // `with`, `except` target, walrus, `del`, a `global` write in another
        // function, a later assignment — may be what the name holds when the
        // call runs; and two roots or a package beside a module of the same
        // path are two answers to the one import.
        if self.python_name_is_ambiguous(file, root)
            || self.python_absolute_module_files(file, module_path).len() > 1
        {
            return None;
        }
        // The same dotted path under two source roots (`src/a/b/c.py` and
        // `lib/a/b/c.py`): which one the import names is a `sys.path` fact the
        // walk's root list may not hold, so a second file of that path is a
        // second answer.
        let tail = module_path.replace('.', "/");
        let module = format!("{tail}.py");
        let package = format!("{tail}/__init__.py");
        let nested_module = format!("/{module}");
        let nested_package = format!("/{package}");
        let namesakes = self
            .file_symbols
            .keys()
            .filter(|path| {
                **path == module
                    || **path == package
                    || path.ends_with(&nested_module)
                    || path.ends_with(&nested_package)
            })
            .count();
        if namesakes > 1 {
            return None;
        }
        self.python_dotted_declaration(file, scope, root, &format!("{middle}.{member}"), &binds)
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
            && !self.python_stdlib_name_is_shadowed(file, module))
        .then_some(qualified)
    }

    /// Whether the corpus declares a module that `import module`, written in
    /// `file`, reaches instead of the standard library's.
    ///
    /// Python 3 imports are absolute: `from typing import Generic` in
    /// `pkg/c.py` is the standard library's even when `pkg/typing.py` exists,
    /// because `pkg` is a package and its siblings are named `pkg.typing`. A
    /// module reached through the repository root, `src/` or a manifest's
    /// source roots shadows it, and so does one beside the importer in a
    /// directory that is no package (a script's directory is on `sys.path`).
    fn python_stdlib_name_is_shadowed(&self, file: &str, module: &str) -> bool {
        let Some(found) = self.resolve_import_path(file, "python", module) else {
            return false;
        };
        let dir = Self::parent_dir(file);
        let beside = [
            format!("{dir}/{module}.py"),
            format!("{dir}/{module}/__init__.py"),
        ];
        let at_a_root = [
            format!("{module}.py"),
            format!("{module}/__init__.py"),
            format!("src/{module}.py"),
            format!("src/{module}/__init__.py"),
        ];
        if !beside.contains(&found) || at_a_root.contains(&found) {
            return true;
        }
        !self
            .file_symbols
            .contains_key(&format!("{dir}/__init__.py"))
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
        // An `__all__` built any way but one literal list exports what the
        // module computes, which no fact here reads.
        if self.python_dynamic_all.contains(&module) {
            return true;
        }
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
    /// (where C3 has no answer — Python raises `TypeError` there) are
    /// [`MroStop::Abstain`]; a bound hit is [`MroStop::Capped`], which names the
    /// bound.
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
        if depth > Self::PYTHON_MRO_MAX_DEPTH {
            return Err(MroStop::Capped(format!(
                "python heritage walk stopped at its cap of {} bases deep",
                Self::PYTHON_MRO_MAX_DEPTH
            )));
        }
        if memo.len() > Self::PYTHON_MRO_MAX_NODES {
            return Err(MroStop::Capped(format!(
                "python heritage walk stopped at its cap of {} classes linearized",
                Self::PYTHON_MRO_MAX_NODES
            )));
        }
        if !in_progress.insert(node.clone()) {
            return Err(MroStop::Abstain);
        }
        // `if c: class C(A)` / `else: class C(B)` declares one identity twice
        // and the heritage table unions both statements' bases. Whichever
        // statement ran is not knowable, so no linearization is read from it.
        if self.python_class_declarations(file, identity) > 1 {
            return Err(MroStop::Abstain);
        }
        let key = Self::python_type_key(identity).to_string();
        // A class written inside a function files its heritage under its bare
        // name, not under the dotted identity this walk reads by, so a class
        // with several bases there would be linearized without them. Such a
        // class is not scoped; the flat table answers for it, and only when its
        // heritage is a single chain ([`Self::python_flat_heritage_is_linear`]).
        // A module-level class of the same bare name shares that entry, and then
        // neither is read.
        let bare = key.rsplit('.').next().unwrap_or(&key).to_string();
        // The root is named by the bare name its own heritage is filed under
        // (`self_type_at`), so only a class reached through a base, which the
        // walk names by its dotted identity, is affected.
        if depth > 0
            && self
                .python_class_enclosers
                .get(&(file.clone(), bare.clone()))
                .is_some_and(|enclosers| enclosers.iter().any(|(_, callable)| *callable))
        {
            return Err(if self.python_class_declarations(file, &bare) > 0 {
                MroStop::Abstain
            } else {
                MroStop::NotScoped
            });
        }
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
            } else if self.python_base_is_rebound_around(file, identity, base) {
                // The class is written inside a scope that binds the base's
                // first name itself, so the module's reading of it is not
                // the class statement's.
                bases.push(MroNode::Opaque(base.clone()));
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
    ///
    /// With `sole_declarer`, a second class further along the MRO that also
    /// declares the name abstains as well: C3 does choose the first, but a
    /// `self.m()` written in a class is a call on whichever subclass instance
    /// is running, and the resolver commits to an edge only when one
    /// declaration is all there is. `super().m()` names the next class by
    /// definition and does not ask for this.
    pub(super) fn python_mro_method(
        &self,
        class_file: &str,
        type_key: &str,
        method: &str,
        sole_declarer: bool,
    ) -> MroAnswer {
        let root = MroNode::Class(class_file.to_string(), type_key.to_string());
        let mut memo = BTreeMap::new();
        let mut in_progress = BTreeSet::new();
        let linear = match self.python_linearize(&root, 0, &mut memo, &mut in_progress) {
            Ok(linear) => linear,
            Err(MroStop::NotScoped) => return MroAnswer::NotScoped,
            Err(MroStop::Abstain) => return MroAnswer::Abstain,
            Err(MroStop::Capped(reason)) => return MroAnswer::Capped(reason),
        };
        for (position, node) in linear.iter().enumerate().skip(1) {
            match node {
                MroNode::Opaque(_) => return MroAnswer::Abstain,
                MroNode::Class(file, identity) => {
                    let key = Self::python_type_key(identity);
                    match self.python_own_methods(file, key, method).as_slice() {
                        [] => {}
                        [(path, symbol)] => {
                            let shadowed = sole_declarer
                                && linear[position + 1..].iter().any(|later| match later {
                                    MroNode::Class(later_file, later_identity) => !self
                                        .python_own_methods(
                                            later_file,
                                            Self::python_type_key(later_identity),
                                            method,
                                        )
                                        .is_empty(),
                                    MroNode::Opaque(_) => false,
                                });
                            if shadowed {
                                return MroAnswer::Abstain;
                            }
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
    ///
    /// Only the zero-argument form and `super(Enclosing, self)` name the
    /// enclosing class; `super(Other, self)` starts the walk after `Other` in
    /// the instance's MRO, which is not this class's bases.
    pub(super) fn python_super_method(
        &self,
        ext: &Extraction,
        call: &ExtractedCall,
        caller: &str,
        method: &str,
    ) -> Option<PythonMethod> {
        let file = ext.file_path.as_str();
        let enclosing = self.self_type_at(file, LangFamily::Python, caller)?;
        if !python_super_names_enclosing(ext, call, &enclosing) {
            return None;
        }
        self.python_inherited_from(file, &enclosing, method)
    }

    /// The class a `cls(...)` call inside a method of it constructs.
    pub(super) fn python_cls_target(&self, file: &str, caller: &str) -> Option<(String, String)> {
        let enclosing = self.self_type_at(file, LangFamily::Python, caller)?;
        self.python_class_named(file, Some(caller), &enclosing)
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
        self.python_scope_lookup(file, scope, root, depth).is_some()
    }

    /// What binds `root` in the innermost function that encloses `scope`
    /// (walking out from it, class bodies skipped as Python skips them): an
    /// import written in that function ([`ScopeHit::Import`], with what it
    /// resolved to, or nothing when it resolved to no file) or any other
    /// binding the extractor records for it ([`ScopeHit::Local`]). `None` when
    /// no enclosing function binds it, so the module's binding is the one read.
    ///
    /// The one owner of "is this name bound in this scope": the base lookup,
    /// the field-initializer shadow check and the call path's import lookup
    /// ([`Self::python_import_binding`]) all ask it.
    pub(super) fn python_scope_lookup(
        &self,
        file: &str,
        scope: &str,
        root: &str,
        depth: usize,
    ) -> Option<ScopeHit<'_>> {
        let mut current = scope.to_string();
        let mut callable = depth > 0;
        let mut own_body = depth == 0;
        for _ in 0..Self::PYTHON_MRO_MAX_DEPTH {
            let class_tail = if callable {
                None
            } else {
                self.python_class_scope(file, &current)
            };
            // A class body's names are not in scope inside its methods, but a
            // read written in the class body itself sees its imports.
            if class_tail.is_some() && own_body {
                if let Some(bound) = self
                    .python_scoped_import_bindings
                    .get(&(file.to_string(), current.clone()))
                    .and_then(|names| names.get(root))
                {
                    return Some(ScopeHit::Import(bound.as_ref()));
                }
            }
            own_body = false;
            if class_tail.is_none() {
                if let Some(bound) = self
                    .python_scoped_import_bindings
                    .get(&(file.to_string(), current.clone()))
                    .and_then(|names| names.get(root))
                {
                    return Some(ScopeHit::Import(bound.as_ref()));
                }
                if self.scope_declares_local(file, &current, root)
                    || self.python_nested_declares(file, &current, root)
                {
                    return Some(ScopeHit::Local);
                }
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
                        if let Some(hit) =
                            self.python_scope_lookup(file, enclosing, root, depth + 1)
                        {
                            return Some(hit);
                        }
                    } else {
                        classes.push(enclosing.clone());
                    }
                }
                match classes.as_slice() {
                    [] => return None,
                    [only] => {
                        current = only.clone();
                        continue;
                    }
                    _ => return None,
                }
            }
            let Some(parent) = self
                .symbol_parents
                .get(&(file.to_string(), current.clone()))
                .filter(|parent| parent.as_str() != file && **parent != current)
            else {
                return None;
            };
            current = parent.clone();
        }
        Some(ScopeHit::Local)
    }

    /// The `(target file, declared name)` an import binds `name` to where it is
    /// read in `scope` of `file`.
    ///
    /// `scope` is the callable the name is read in; `Some(file)` (or `None`,
    /// which is what a statement outside every function and class records)
    /// reads at module scope, where only the module's own binding exists
    /// whatever a function elsewhere imports. An import in an enclosing
    /// function wins over the module's, and when that import resolved to no
    /// file, or the function binds the name some other way, the answer is
    /// none: it never falls through to the module's import of the same name.
    pub(super) fn python_import_binding(
        &self,
        file: &str,
        scope: Option<&str>,
        name: &str,
    ) -> Option<&(String, String)> {
        if let Some(scope) = scope.filter(|scope| *scope != file) {
            match self.python_scope_lookup(file, scope, name, 0) {
                Some(ScopeHit::Import(bound)) => return bound,
                Some(ScopeHit::Local) => return None,
                None => {}
            }
        }
        let flat = self.import_bindings.get(file)?.get(name)?;
        // Only functions bind it, and to different things: the flat table
        // holds one of them, and no function enclosing this read is the one.
        (!self
            .python_ambiguous_scoped_imports
            .contains(&(file.to_string(), name.to_string())))
        .then_some(flat)
    }
}

/// Whether a Python module's `__all__` is anything but the one literal list the
/// extractor reads names from.
///
/// The extractor files the strings of a literal `__all__ = [..]` (and `+= [..]`)
/// as locals of the scope `{file}::__all__`, and keeps nothing of any other
/// way to build the list, so a reader of those names alone would take a
/// module whose list is also extended by a call as exporting only the
/// literal part. The other ways are each recorded under another fact:
///
/// * a call or comprehension on the right of `=` / `+=` is a reference assigned
///   to `__all__`, and a concatenation of literals is one too (its strings
///   become `EntryName` references assigned to it);
/// * `__all__.extend(..)`, `.append(..)` and every other method on it is a
///   call whose receiver is `__all__`;
/// * `other.__all__` read at module level is a `Name` with a receiver;
/// * a `def __all__` or `class __all__` is a symbol of that name, whose
///   locals share the `{file}::__all__` scope key with the literal list;
/// * the extractor reads those names from the source *text*, wherever
///   `__all__ = [..]` is written: a docstring, a function body or a class body
///   file names under the same key. A name with no string literal at module
///   level is therefore not a name a module-level `__all__` lists;
/// * the string `"__all__"` written anywhere (`globals()["__all__"]`,
///   `setattr(module, "__all__", ..)`) is the list being read or set by name.
///
/// Any of them means `import *` may take any name the module binds.
pub(super) fn python_all_is_dynamic(ext: &Extraction) -> bool {
    let all_scope = format!("{}::__all__", ext.file_path);
    ext.scope_locals.iter().any(|(scope, name)| {
        *scope == all_scope
            && !ext.literals.iter().any(|literal| {
                literal.value == *name && literal.enclosing_qualified_name.is_empty()
            })
    }) || ext.symbols.iter().any(|symbol| {
        symbol.name == "__all__"
            && matches!(symbol.kind, SymbolKind::Function | SymbolKind::Class)
            && symbol.parent_symbol.as_deref() == Some(ext.file_path.as_str())
    }) || ext.references.iter().any(|reference| {
        reference.enclosing_symbol.is_none()
            && (reference.assigned_to.as_deref() == Some("__all__")
                // `other.__all__` read at module level and handed to no call
                // (`['x'] + other.__all__`) is an operand of the list being
                // built; one inside a call (`len(os.__all__)`,
                // `os.__all__.copy()`) is a read of someone else's list.
                || (reference.kind == ReferenceKind::Name
                    && reference.name == "__all__"
                    && reference.receiver_expr.is_some()
                    && !ext.calls.iter().any(|call| {
                        call.span.start_byte <= reference.span.start_byte
                            && reference.span.end_byte <= call.span.end_byte
                    })))
    }) || ext.calls.iter().any(|call| {
        // A mutating method on the bare name `__all__`, from any scope: a
        // function the module runs at import can append to it as well as the
        // module body can. `globals()["__all__"].append(..)` spells the
        // receiver around it. `os.__all__`, `my__all__` and a read such as
        // `__all__.__len__()` are not mutations of this module's list.
        const MUTATORS: [&str; 9] = [
            "append", "extend", "insert", "remove", "pop", "clear", "sort", "reverse", "__iadd__",
        ];
        call.receiver_expr.as_deref().is_some_and(|receiver| {
            (receiver == "__all__"
                || (receiver.starts_with("globals()[")
                    && (receiver.contains("\"__all__\"") || receiver.contains("'__all__'"))))
                && MUTATORS.contains(&call.callee_name.as_str())
        })
    }) || python_all_named_by_string(ext)
}

/// Whether the string `"__all__"` is the key of `globals()[..]` or the
/// attribute name of `setattr(module, "__all__", ..)`: the two spellings that
/// set the list by name. Any other use of the string (`hasattr`, a log
/// message, an assertion) reads or says nothing about this module's list.
fn python_all_named_by_string(ext: &Extraction) -> bool {
    ext.literals
        .iter()
        .filter(|literal| literal.value == "__all__")
        .any(|literal| {
            ext.calls.iter().any(|call| {
                call.receiver_expr.is_none()
                    && match call.callee_name.as_str() {
                        // `globals()["__all__"]`: the string follows the call.
                        "globals" => {
                            call.span.end_byte <= literal.start_byte
                                && literal.start_byte <= call.span.end_byte + 3
                        }
                        // `setattr(module, "__all__", ..)`: inside the call.
                        "setattr" => {
                            call.span.start_byte <= literal.start_byte
                                && literal.start_byte < call.span.end_byte
                        }
                        _ => false,
                    }
            })
        })
}

/// Whether the `super(..)` a `super(..).m()` call is made through starts the
/// walk at the class it is written in: `super()`, or `super(Enclosing, x)` with
/// the enclosing class's own name as its first argument.
///
/// The extractor keeps no arguments on a call, but it does keep every
/// identifier it reads as a `Name` reference at its own span, so the arguments
/// of the inner `super` call are the `Name`s inside its span. No names is the
/// zero-argument form. Two names whose first is the enclosing class's bare
/// name, written as `super(Name, x)` with at most one separator between them,
/// is the explicit form of the same walk; anything else (another class, a
/// dotted or computed first argument, a third argument) names a class this
/// resolver does not follow and is refused.
fn python_super_names_enclosing(ext: &Extraction, call: &ExtractedCall, enclosing: &str) -> bool {
    let Some(inner) = ext.calls.iter().find(|inner| {
        inner.callee_name == "super"
            && inner.receiver_expr.is_none()
            && inner.span.start_byte == call.span.start_byte
            && inner.span.end_byte <= call.span.end_byte
    }) else {
        // No `super(..)` call was recorded under this one: the receiver was
        // the bare name `super`, read as the zero-argument form before.
        return true;
    };
    let mut names: Vec<&ExtractedReference> = ext
        .references
        .iter()
        .filter(|reference| {
            reference.kind == ReferenceKind::Name
                && reference.span.start_byte >= inner.span.start_byte
                && reference.span.end_byte <= inner.span.end_byte
        })
        .collect();
    names.sort_by_key(|reference| reference.span.start_byte);
    match names.as_slice() {
        [] => true,
        [first, second] => {
            let own = enclosing.rsplit('.').next().unwrap_or(enclosing);
            first.name == own
                && first.span.start_byte == inner.span.start_byte + "super(".len()
                && second.span.start_byte <= first.span.end_byte + 2
                && second.span.end_byte + 1 == inner.span.end_byte
        }
        _ => false,
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
/// as m` the member `n` of `a`.
///
/// Module scope only: an import written inside a function binds that
/// function's local (the extractor files it in `scope_locals`, which the scope
/// walk reads), and one written in a class body binds a class attribute
/// ([`Resolver::python_scope_lookup`]). Neither rebinds the module's name, so a
/// class statement at module level reads the module's imports alone.
pub(super) fn python_import_binds(
    ext: &Extraction,
) -> BTreeMap<String, BTreeSet<PythonImportBind>> {
    let mut binds: BTreeMap<String, BTreeSet<PythonImportBind>> = BTreeMap::new();
    // Where each name is first imported, and each `from m import *`: a star
    // written after a name's import may rebind it.
    let mut first_import: BTreeMap<String, usize> = BTreeMap::new();
    let mut stars: Vec<(usize, &str)> = Vec::new();
    let containers = python_scope_containers(ext);
    for imp in &ext.imports {
        if imp.path_load.is_some() || python_innermost_container(&containers, &imp.span).is_some() {
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

/// The functions and methods of a Python file a decorator may wrap, by
/// qualified name.
///
/// The extraction does not link a decorator to what it decorates: `@many` is a
/// `Name` reference at the enclosing scope, ahead of the `def`. So the test is
/// the one the spans allow, and it errs toward abstaining: a callable counts as
/// decorated when any such reference (a name or call that is not the target of
/// an assignment) begins after the previous symbol ended and inside the
/// callable's own enclosing scope, and ends before the `def`. A statement
/// like `register(x)` between two functions therefore also counts, which costs
/// a return type, never a wrong edge.
///
/// Transparent decorators return the wrapped function's value unchanged, so
/// the written return type is still what a call yields: `functools.cache` and
/// `functools.lru_cache` (by their `functools` identity through this file's
/// imports — `from functools import cache as memo`, `import functools`), and
/// the builtins `staticmethod` and `classmethod` when nothing in the file
/// rebinds them. A same-named function the file defines or imports from
/// elsewhere is opaque. A base class in a class header is a `Name` reference
/// over the span of its `Heritage` reference, and is no decorator.
pub(super) fn python_decorated_callables(ext: &Extraction) -> Vec<String> {
    let binds = python_import_binds(ext);
    let file_level: BTreeSet<&str> = ext
        .symbols
        .iter()
        .filter(|sym| sym.parent_symbol.as_deref() == Some(ext.file_path.as_str()))
        .map(|sym| sym.name.as_str())
        .collect();
    let sole_bind = |name: &str| -> Option<&PythonImportBind> {
        let bound = binds.get(name)?;
        let mut only = bound.iter();
        match (only.next(), only.next()) {
            (Some(bind), None) if !file_level.contains(name) => Some(bind),
            _ => None,
        }
    };
    let is_functools_module = |name: &str| {
        matches!(
            sole_bind(name),
            Some(PythonImportBind::Package(module) | PythonImportBind::Module(module))
                if module == "functools"
        )
    };
    let is_cache_name = |name: &str| matches!(name, "cache" | "lru_cache");
    let functools_roots: BTreeSet<usize> = ext
        .references
        .iter()
        .filter(|r| {
            r.kind == ReferenceKind::Name
                && r.receiver_expr.is_none()
                && is_functools_module(&r.name)
        })
        .map(|r| r.span.start_byte)
        .collect();
    let transparent = |r: &ExtractedReference| -> bool {
        let name = r.name.as_str();
        match r.receiver_expr.as_deref() {
            Some(root) => is_cache_name(name) && is_functools_module(root),
            None => {
                (matches!(name, "staticmethod" | "classmethod")
                    && !file_level.contains(name)
                    && !binds.contains_key(name))
                    || matches!(
                        sole_bind(name),
                        Some(PythonImportBind::Member(member))
                            if member == "functools.cache" || member == "functools.lru_cache"
                    )
                    || (is_cache_name(name) && functools_roots.contains(&r.span.start_byte))
                    || (r.kind == ReferenceKind::Name && is_functools_module(name))
            }
        }
    };
    let base_spans: BTreeSet<(usize, usize)> = ext
        .references
        .iter()
        .filter(|r| r.kind == ReferenceKind::Heritage)
        .map(|r| (r.span.start_byte, r.span.end_byte))
        .collect();
    let mut gaps: Vec<(usize, usize)> = ext
        .references
        .iter()
        .filter(|r| {
            matches!(r.kind, ReferenceKind::Name | ReferenceKind::Call)
                && r.assigned_to.is_none()
                && !transparent(r)
                && !(r.kind == ReferenceKind::Name
                    && base_spans.contains(&(r.span.start_byte, r.span.end_byte)))
        })
        .map(|r| (r.span.start_byte, r.span.end_byte))
        .collect();
    gaps.sort_unstable();
    let mut ends: Vec<usize> = ext
        .symbols
        .iter()
        .filter(|sym| sym.kind != SymbolKind::File)
        .map(|sym| sym.span.end_byte)
        .collect();
    ends.sort_unstable();
    let starts: BTreeMap<&str, usize> = ext
        .symbols
        .iter()
        .map(|sym| (sym.qualified_name.as_str(), sym.span.start_byte))
        .collect();
    let mut decorated = Vec::new();
    for sym in &ext.symbols {
        if !matches!(sym.kind, SymbolKind::Function | SymbolKind::Method) {
            continue;
        }
        let def_start = sym.span.start_byte;
        let previous_end = ends
            .partition_point(|end| *end <= def_start)
            .checked_sub(1)
            .map_or(0, |at| ends[at]);
        let parent_start = sym
            .parent_symbol
            .as_deref()
            .filter(|parent| *parent != ext.file_path)
            .and_then(|parent| starts.get(parent).copied())
            .unwrap_or(0);
        let lower = previous_end.max(parent_start);
        let first = gaps.partition_point(|(start, _)| *start < lower);
        let wrapped = gaps[first..]
            .iter()
            .take_while(|(start, _)| *start < def_start)
            .any(|(_, end)| *end <= def_start);
        if wrapped {
            decorated.push(sym.qualified_name.clone());
        }
    }
    decorated
}

/// A Python file's calls grouped by the byte offset they begin at. Two calls
/// share a start when one is the receiver of the other (`f(x).m()`), which is
/// how [`Resolver::python_call_result_type`] finds the inner call.
pub(super) fn python_calls_by_start(ext: &Extraction) -> BTreeMap<usize, Vec<&ExtractedCall>> {
    let mut by_start: BTreeMap<usize, Vec<&ExtractedCall>> = BTreeMap::new();
    for call in &ext.calls {
        by_start.entry(call.span.start_byte).or_default().push(call);
    }
    by_start.retain(|_, calls| calls.len() > 1);
    by_start
}

/// A Python file's attribute reads (`Name` references with a receiver), keyed
/// by `(receiver, name)` with their start offsets in source order. See
/// [`python_field_root_binding`].
pub(super) fn python_field_reads(ext: &Extraction) -> BTreeMap<(&str, &str), Vec<usize>> {
    let mut reads: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for reference in &ext.references {
        if reference.kind != ReferenceKind::Name {
            continue;
        }
        if let Some(receiver) = reference.receiver_expr.as_deref() {
            reads
                .entry((receiver, reference.name.as_str()))
                .or_default()
                .push(reference.span.start_byte);
        }
    }
    for starts in reads.values_mut() {
        starts.sort_unstable();
    }
    reads
}

/// The binding of `root` where the call `root.field.m()` reads `field`.
///
/// A parameter has a binding at every use, so the call's own start finds it. A
/// local assigned from a factory (`result = train()`) is bound only at the
/// attribute read the extractor records inside the call — at `field`'s own
/// offset, after `root.` — and without it the root is untyped here, left to
/// the file-wide maps, which hold a *namesake in another function*. The read
/// is the first one of `root.field` that begins inside the call, which is the
/// receiver's own: the receiver is written before the arguments.
pub(super) fn python_field_root_binding<'a>(
    ext: &'a Extraction,
    reads: &BTreeMap<(&str, &str), Vec<usize>>,
    call: &ExtractedCall,
    root: &str,
    field: &str,
) -> Option<&'a LocalBinding> {
    let starts = reads.get(&(root, field))?;
    let first = starts.partition_point(|start| *start < call.span.start_byte);
    let at = *starts.get(first).filter(|at| **at < call.span.end_byte)?;
    ext.local_binding_at(at, root)
}

/// The spans of every function, method and class a Python file declares, with
/// the kind: the scopes an import can be written inside.
pub(super) fn python_scope_containers(ext: &Extraction) -> ScopeIndex<'_> {
    let mut spans: Vec<ScopeSpan<'_>> = ext
        .symbols
        .iter()
        .filter(|sym| {
            matches!(
                sym.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Class
            )
        })
        .map(|sym| {
            (
                sym.span.start_byte,
                sym.span.end_byte,
                sym.kind,
                sym.qualified_name.as_str(),
            )
        })
        .collect();
    // Outer scopes first at one start, so a stack sweep finds each parent.
    spans.sort_by_key(|(start, end, _, _)| (*start, std::cmp::Reverse(*end)));
    let mut parents: Vec<Option<usize>> = Vec::with_capacity(spans.len());
    let mut open: Vec<usize> = Vec::new();
    for (index, (start, _, _, _)) in spans.iter().enumerate() {
        while open.last().is_some_and(|top| spans[*top].1 <= *start) {
            open.pop();
        }
        parents.push(open.last().copied());
        open.push(index);
    }
    ScopeIndex { spans, parents }
}

/// A function, method or class of a Python file: its span, kind and qualified
/// name.
pub(super) type ScopeSpan<'a> = (usize, usize, SymbolKind, &'a str);

/// The functions, methods and classes of a Python file, ordered by position
/// with each one's enclosing scope, so the innermost scope around a span is a
/// binary search and a short walk rather than a scan of every symbol.
pub(super) struct ScopeIndex<'a> {
    spans: Vec<ScopeSpan<'a>>,
    parents: Vec<Option<usize>>,
}

/// The kind and qualified name of the innermost function, method or class
/// that contains `span`, or `None` for a statement at module scope.
pub(super) fn python_innermost_container<'a>(
    containers: &ScopeIndex<'a>,
    span: &Span,
) -> Option<(SymbolKind, &'a str)> {
    let after = containers
        .spans
        .partition_point(|(start, _, _, _)| *start <= span.start_byte);
    let mut current = after.checked_sub(1);
    while let Some(index) = current {
        let (_, end, kind, name) = containers.spans[index];
        if span.end_byte <= end {
            return Some((kind, name));
        }
        current = containers.parents[index];
    }
    None
}
