//! Which declaration a member of a JS/TS module-scope object is.
//!
//! `export const debugLogger = { ...core, clear: clearLogs }` has members its
//! literal does not spell (`core`'s) and members whose value is another
//! declaration (`clearLogs`). The extractor records how each const object is
//! built and every later write that can change it, in source order
//! ([`JsObjectFact`]); this reads those steps the way JavaScript applies them,
//! last write first:
//!
//! * a key written in place, or forwarded from an identifier, is that
//!   declaration — an explicit key after a spread beats it, a later spread
//!   beats an earlier one, a later `w.k = f` beats both;
//! * a spread, `Object.assign` source or alias is followed through the import
//!   binding to the source object's own steps, up to
//!   [`Resolver::REEXPORT_CHAIN_MAX_DEPTH`] hops;
//! * anything the extraction could not name — a spread of a call's result, a
//!   computed key, a write through an escaped reference, a source in a file
//!   that is not indexed — may define any key, so the walk abstains for every
//!   key it has not yet answered, and a cycle abstains too;
//! * a write that may not run (under a condition, or inside a callable) makes
//!   the keys it touches uncertain: abstain for them, never bind through it.
//!
//! **When a use sees a step.** A use at module level sees only the steps that
//! precede it in the file; a use inside a function body runs later, so it sees
//! every unconditional module-level step. A write inside a callable may have
//! run before any use, so every use abstains for the keys it touches. A spread
//! copies its source as the source stood at the spread, so a same-file source
//! is read only up to the spread. Writes made from *another* file are not
//! visible here (the extraction of that file does not name this one's objects).
//!
//! An abstention is not "the key is absent": only a walk that reaches the first
//! step having met no unknown says [`ObjectMember::Absent`].

use super::*;

/// One file's object facts, indexed once by const name so a call is a lookup
/// and not a scan of the file.
#[derive(Default)]
pub(super) struct FileObjects {
    declarations: BTreeMap<String, Vec<JsObjectFact>>,
    /// Writes and escapes, by the name they were written through, in source
    /// order.
    writes: BTreeMap<String, Vec<JsObjectFact>>,
    /// Plain-identifier alias targets: `const a = w` files `a` under `w`.
    aliases: BTreeMap<String, Vec<String>>,
    /// Locals this file binds with a default import (`import lg from './m'`):
    /// a value, not a module, whose members are the default export's.
    default_imports: BTreeSet<String>,
}

impl FileObjects {
    fn new(ext: &Extraction) -> Self {
        let mut index = Self::default();
        for import in &ext.imports {
            for (name, local) in import.imported_names.iter().zip(&import.local_names) {
                if name == "default" {
                    index.default_imports.insert(local.clone());
                }
            }
            // The fallback spelling of `import X from './m'` records only the
            // alias, with no imported names.
            if import.imported_names.is_empty() {
                if let Some(alias) = &import.alias {
                    index.default_imports.insert(alias.clone());
                }
            }
        }
        let facts = &ext.js_objects;
        for fact in facts {
            if fact.declaration {
                if let Some(target) = fact.alias_of.as_deref().filter(|t| !t.contains('.')) {
                    index
                        .aliases
                        .entry(target.to_string())
                        .or_default()
                        .push(fact.name.clone());
                }
                index
                    .declarations
                    .entry(fact.name.clone())
                    .or_default()
                    .push(fact.clone());
            } else {
                index
                    .writes
                    .entry(fact.name.clone())
                    .or_default()
                    .push(fact.clone());
            }
        }
        index
    }

    /// `name` and every name that is, transitively, an alias of it declared in
    /// this file. Bounded by the file's own alias count and cycle-safe.
    fn family<'a>(&'a self, name: &'a str) -> Vec<&'a str> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut order = Vec::new();
        let mut queue = vec![name];
        while let Some(next) = queue.pop() {
            if !seen.insert(next) {
                continue;
            }
            order.push(next);
            if let Some(children) = self.aliases.get(next) {
                queue.extend(children.iter().map(String::as_str));
            }
        }
        order
    }
}

/// What a name used as a spread source, alias or receiver denotes.
enum ObjectRef {
    /// A module-scope const object this corpus declares.
    Object(String, String),
    /// A whole module (`import * as core`, `require('./core')`): its members
    /// are the file's own top-level declarations.
    Namespace(String),
    Unknown,
}

pub(super) enum ObjectMember {
    /// The member's declaration: `(file, symbol identity)`.
    Bound(String, String),
    /// No step defines it and every step was readable.
    Absent,
    /// A step that may define it could not be read.
    Abstain,
}

struct Step<'a> {
    part: &'a JsObjectPart,
    conditional: bool,
    /// Byte offset the step runs at, in its own file.
    at: usize,
}

impl Resolver {
    pub(super) fn index_js_objects(&mut self, ext: &Extraction) {
        if !ext.js_objects.is_empty() {
            self.js_objects
                .insert(ext.file_path.clone(), FileObjects::new(ext));
        }
    }

    fn declared_object(&self, file: &str, name: &str) -> bool {
        self.js_objects
            .get(file)
            .is_some_and(|objects| objects.declarations.contains_key(name))
    }

    /// A value `name` that module `module_file` publishes, as an object this
    /// corpus declares — directly or through a re-export chain.
    fn object_in_module(&self, module_file: &str, name: &str) -> ObjectRef {
        if self.declared_object(module_file, name) {
            return ObjectRef::Object(module_file.to_string(), name.to_string());
        }
        if let Some((file, declared)) = self
            .reexport_chains
            .get(&format!("{module_file}::{name}"))
            .and_then(|terminal| terminal.rsplit_once("::"))
        {
            if self.declared_object(file, declared) {
                return ObjectRef::Object(file.to_string(), declared.to_string());
            }
        }
        ObjectRef::Unknown
    }

    /// What `reference` — `name` or `ns.name`, as written in `file` — denotes.
    fn object_ref(&self, file: &str, reference: &str) -> ObjectRef {
        if let Some((root, member)) = reference.split_once('.') {
            if self.is_value_import(file, root) {
                return ObjectRef::Unknown;
            }
            return match self.import_bindings.get(file).and_then(|b| b.get(root)) {
                Some((module_file, _)) => self.object_in_module(module_file, member),
                None => ObjectRef::Unknown,
            };
        }
        if self.declared_object(file, reference) {
            return ObjectRef::Object(file.to_string(), reference.to_string());
        }
        if let Some((declaring_file, declared)) = self
            .value_imports
            .get(&(file.to_string(), reference.to_string()))
        {
            return self.object_in_module(declaring_file, declared);
        }
        let Some((module_file, _)) = self
            .import_bindings
            .get(file)
            .and_then(|bindings| bindings.get(reference))
        else {
            return ObjectRef::Unknown;
        };
        let is_default = self
            .js_objects
            .get(file)
            .is_some_and(|objects| objects.default_imports.contains(reference));
        if !is_default {
            return ObjectRef::Namespace(module_file.clone());
        }
        // A default import names the module's default export, which is an
        // object only when `export default name` points at a const the file
        // declares; an anonymous default has no name to follow.
        let declared = self.default_exports.get(module_file).map(String::as_str);
        match declared {
            Some(declared) => self.object_in_module(module_file, declared),
            None => ObjectRef::Unknown,
        }
    }

    /// The declaration a forwarded identifier names in `file`: a named import,
    /// or a top-level declaration of the file itself.
    fn forwarded_declaration(&self, file: &str, identifier: &str) -> ObjectMember {
        let named = match self
            .import_bindings
            .get(file)
            .and_then(|bindings| bindings.get(identifier))
        {
            Some((target_file, target_symbol)) => {
                self.lookup_in_package(target_file, target_symbol)
            }
            None => self.lookup_in_package(file, identifier),
        };
        match named {
            Some((found_file, symbol)) => ObjectMember::Bound(found_file, symbol),
            None => ObjectMember::Abstain,
        }
    }

    fn object_member_walk(
        &self,
        file: &str,
        value: &str,
        member: &str,
        limit: Option<usize>,
        trail: &mut Vec<(String, String)>,
    ) -> ObjectMember {
        let key = (file.to_string(), value.to_string());
        if trail.len() >= Self::REEXPORT_CHAIN_MAX_DEPTH || trail.contains(&key) {
            return ObjectMember::Abstain;
        }
        let Some(objects) = self.js_objects.get(file) else {
            return ObjectMember::Abstain;
        };
        let Some([declaration]) = objects.declarations.get(value).map(Vec::as_slice) else {
            // Two declarations of one name: which ran last is not a fact the
            // extraction holds.
            return ObjectMember::Abstain;
        };

        // An alias is the object it names, not a copy: delegate to it. A write
        // through an alias lands in the target's own family when the target is
        // in this file; when it is in another file those writes are not
        // visible there, so the answer is withheld.
        if let Some(target) = declaration.alias_of.as_deref() {
            if let ObjectRef::Object(target_file, target_value) = self.object_ref(file, target) {
                let same_file = target_file == file;
                if !same_file
                    && objects
                        .family(value)
                        .iter()
                        .any(|name| objects.writes.contains_key(*name))
                {
                    return ObjectMember::Abstain;
                }
                trail.push(key);
                let answer = self.object_member_walk(
                    &target_file,
                    &target_value,
                    member,
                    if same_file { limit } else { None },
                    trail,
                );
                trail.pop();
                return answer;
            }
        }

        let mut steps: Vec<Step> = declaration
            .parts
            .iter()
            .map(|part| Step {
                part,
                conditional: false,
                at: declaration.start_byte,
            })
            .collect();
        let mut writes: Vec<&JsObjectFact> = objects
            .family(value)
            .into_iter()
            .filter_map(|name| objects.writes.get(name))
            .flatten()
            .filter(|write| {
                write.in_callable
                    || (write.start_byte > declaration.start_byte
                        && limit.is_none_or(|limit| write.start_byte < limit))
            })
            .collect();
        // Callable writes run at an unknown time: last, so they outrank
        // nothing they could not already overwrite, and conditional.
        writes.sort_by_key(|write| (write.in_callable, write.start_byte));
        for write in writes {
            steps.extend(write.parts.iter().map(|part| Step {
                part,
                conditional: write.conditional || write.in_callable,
                at: write.start_byte,
            }));
        }
        trail.push(key);
        let answer = self.apply_steps(file, member, &steps, trail);
        trail.pop();
        answer
    }

    fn apply_steps(
        &self,
        file: &str,
        member: &str,
        steps: &[Step],
        trail: &mut Vec<(String, String)>,
    ) -> ObjectMember {
        for step in steps.iter().rev() {
            match step.part {
                JsObjectPart::Inline(key) if key == member => {
                    if step.conditional {
                        return ObjectMember::Abstain;
                    }
                    // The extractor files a member under its owner's own
                    // identity, which is the file-qualified name; the owner is
                    // the declaration this step belongs to.
                    return self.inline_member(file, member, trail);
                }
                JsObjectPart::Forward {
                    key,
                    value: forwarded,
                } if key == member => {
                    if step.conditional {
                        return ObjectMember::Abstain;
                    }
                    return self.forwarded_declaration(file, forwarded);
                }
                JsObjectPart::Opaque(key) if key == member => return ObjectMember::Abstain,
                JsObjectPart::Spread(reference) => {
                    let answer = match self.object_ref(file, reference) {
                        ObjectRef::Object(source_file, source) => {
                            let limit = (source_file == file).then_some(step.at);
                            self.object_member_walk(&source_file, &source, member, limit, trail)
                        }
                        ObjectRef::Namespace(module_file) => {
                            match self.lookup_in_package(&module_file, member) {
                                Some((found_file, symbol)) => {
                                    ObjectMember::Bound(found_file, symbol)
                                }
                                None => ObjectMember::Abstain,
                            }
                        }
                        ObjectRef::Unknown => ObjectMember::Abstain,
                    };
                    match answer {
                        ObjectMember::Absent => {}
                        // A spread that may not run can only add doubt.
                        _ if step.conditional => return ObjectMember::Abstain,
                        decided => return decided,
                    }
                }
                JsObjectPart::Unknown => return ObjectMember::Abstain,
                _ => {}
            }
        }
        ObjectMember::Absent
    }

    /// The symbol a member written in place is: `file::owner.member`, where
    /// `owner` is the innermost object on the walk's trail.
    fn inline_member(&self, file: &str, member: &str, trail: &[(String, String)]) -> ObjectMember {
        let Some((_, owner)) = trail.last() else {
            return ObjectMember::Abstain;
        };
        let qualified = format!("{file}::{owner}.{member}");
        let declared = self.symbol_index.get(&qualified).and_then(|hits| {
            hits.iter()
                .find(|(path, _, _, identity)| path == file && **identity == *qualified)
        });
        match declared {
            Some(_) => ObjectMember::Bound(file.to_string(), qualified),
            None => ObjectMember::Abstain,
        }
    }

    /// What a call `receiver.member()` written in `file` reaches when
    /// `receiver` is a module-scope const object built in place, by spread, by
    /// `Object.assign` or as an alias, in this file or another. `use_at` is the
    /// call's byte offset when it is written at module level, `None` inside a
    /// callable.
    pub(super) fn object_member_target(
        &self,
        file: &str,
        receiver: &str,
        member: &str,
        use_at: Option<usize>,
    ) -> ObjectMember {
        let ObjectRef::Object(object_file, object) = self.object_ref(file, receiver) else {
            return ObjectMember::Absent;
        };
        let limit = if object_file == file { use_at } else { None };
        self.object_member_walk(&object_file, &object, member, limit, &mut Vec::new())
    }
}
