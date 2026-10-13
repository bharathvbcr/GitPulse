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

impl Resolver {
    /// The class `written` denotes in `file`, as `(declaring file, class
    /// identity)`: declared at file level here, or bound by an `import` to a
    /// class declared elsewhere. `None` when the name is neither, or is
    /// declared twice in the file.
    pub(super) fn python_class_named(&self, file: &str, written: &str) -> Option<(String, String)> {
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
        let own: Vec<&(String, String)> = self
            .type_methods
            .get(&(LangFamily::Python, type_key.to_string(), method.to_string()))
            .into_iter()
            .flatten()
            .filter(|(path, _)| *path == class_file)
            .collect();
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
        Ok(self.python_inherited_method(type_key, method))
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
        self.python_inherited_method(&enclosing, method)
    }

    /// The class a `cls(...)` call inside a method of it constructs.
    pub(super) fn python_cls_target(&self, file: &str, caller: &str) -> Option<(String, String)> {
        let enclosing = self.self_type_at(file, LangFamily::Python, caller)?;
        self.python_class_named(file, &enclosing)
    }
}
