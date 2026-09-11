//! Turning a specifier into candidate repository paths.
//!
//! Thirteen languages gained import extraction in W0.3 move 2, and a specifier
//! nothing resolves is worth nothing: `unwired_candidates` asks whether a file
//! has an inbound `Imports` **edge**, and an edge needs a target. Writing
//! thirteen bespoke ladders beside the four that already exist would have
//! meant thirteen places to get `..` normalisation wrong, so this is one table
//! instead.
//!
//! The languages differ in exactly three ways and agree on everything else:
//!
//! * the **separator** their specifier uses between path segments — `.` for the
//!   JVM family and Lua, `\` for PHP, `/` for everything else;
//! * the **extensions** a segment path can end in;
//! * the **roots** a non-relative specifier is resolved against, which is the
//!   language's own build layout (`src/main/java`, `lib`, `contracts`).
//!
//! So the table carries those three and the generator carries the rest. Adding
//! a language is a row, and a row is checked against real extraction by
//! `import_paths_resolve_to_real_files.rs` rather than by review.
//!
//! What this deliberately does **not** do is guess. Every candidate is a path
//! the language's own rules could produce; the resolver takes the first one
//! that names an indexed file and otherwise returns nothing, which leaves the
//! import in the unresolved ledger where it can be counted.

/// How one language spells a path inside an import specifier.
#[derive(Debug, Clone, Copy)]
pub struct ImportPathRule {
    /// Grammar keys this row serves.
    pub langs: &'static [&'static str],
    /// What the specifier writes between path segments. `'/'` means the
    /// specifier is already a path and only needs normalising.
    pub separator: char,
    /// Extensions to append, in order. `""` first means "the specifier already
    /// carries its extension" and is tried before any is added.
    pub extensions: &'static [&'static str],
    /// Directory-index files, for languages where a specifier may name a
    /// directory: Nix's `default.nix`, Lua's `init.lua`.
    pub index_files: &'static [&'static str],
    /// Repository-root prefixes to try, in order, after the importing file's
    /// own directory. `""` is the repository root itself.
    pub roots: &'static [&'static str],
}

/// The one table. Ordered as the languages appear in `langimports`.
pub const IMPORT_PATH_RULES: &[ImportPathRule] = &[
    // The C family. The specifier is already a path with its extension, and
    // the roots are the include directories a build system conventionally
    // passes with `-I`. `<vector>` runs the same ladder and matches nothing,
    // which is the correct answer for a system header.
    ImportPathRule {
        langs: &["c", "cpp", "objc", "cuda"],
        separator: '/',
        extensions: &[""],
        index_files: &[],
        roots: &["", "include", "src", "inc", "lib", "headers"],
    },
    // The JVM family. `com.foo.Bar` is `com/foo/Bar.<ext>` under a source root,
    // which is a rule the language specification fixes for Java and a strong
    // convention for the other two — either way a specifier that maps to no
    // indexed file produces no edge rather than a wrong one.
    ImportPathRule {
        langs: &["java"],
        separator: '.',
        extensions: &[".java"],
        index_files: &[],
        roots: &[
            "",
            "src",
            "src/main/java",
            "app/src/main/java",
            "java",
            "test/java",
            "src/test/java",
        ],
    },
    ImportPathRule {
        langs: &["kotlin"],
        separator: '.',
        extensions: &[".kt", ".kts"],
        index_files: &[],
        roots: &[
            "",
            "src",
            "src/main/kotlin",
            "app/src/main/kotlin",
            "kotlin",
            "src/test/kotlin",
        ],
    },
    ImportPathRule {
        langs: &["scala"],
        separator: '.',
        extensions: &[".scala", ".sc"],
        index_files: &[],
        roots: &["", "src", "src/main/scala", "src/test/scala"],
    },
    // Dart. `package:app/util.dart` is `lib/util.dart` by a rule the Dart
    // tooling fixes; the `package:<name>/` prefix is stripped by the caller
    // before the ladder runs, because the package name is the pubspec's and
    // not a directory.
    ImportPathRule {
        langs: &["dart"],
        separator: '/',
        extensions: &["", ".dart"],
        index_files: &[],
        roots: &["", "lib", "lib/src", "bin", "test"],
    },
    // PHP. Both syntaxes land here: `lib/util.php` from a `require` is already
    // a path, and `App\Foo\Bar` from a `use` becomes one under PSR-4 — a
    // convention of Composer rather than of the language, which is why the
    // roots list is where a PSR-4 `autoload` map conventionally points.
    ImportPathRule {
        langs: &["php"],
        separator: '\\',
        extensions: &["", ".php"],
        index_files: &[],
        roots: &["", "src", "app", "lib", "classes", "includes"],
    },
    // Ruby. `require_relative 'helper'` is resolved against the importing
    // file's directory first, which the generator always tries; `require
    // 'app/helper'` is resolved against the load path, which for a
    // repository's own code is `lib` or the root.
    ImportPathRule {
        langs: &["ruby"],
        separator: '/',
        extensions: &["", ".rb"],
        index_files: &[],
        roots: &["", "lib", "app", "src", "test", "spec"],
    },
    // Lua and Luau. `require("app.util")` is `app/util.lua` by the default
    // `package.path`, whose `?/init.lua` entry is the index file.
    ImportPathRule {
        langs: &["lua", "luau"],
        separator: '.',
        extensions: &["", ".lua", ".luau"],
        index_files: &["init.lua", "init.luau"],
        roots: &["", "src", "lua", "lib"],
    },
    // R. `source("helpers.R")` is a path relative to the working directory,
    // which for a package is the project root and for a script is its own
    // directory; both are tried.
    ImportPathRule {
        langs: &["r"],
        separator: '/',
        extensions: &["", ".R", ".r"],
        index_files: &[],
        roots: &["", "R", "src", "scripts"],
    },
    // Nix. `import ./lib` is `./lib.nix` or `./lib/default.nix`, and both are
    // rules of the language rather than conventions.
    ImportPathRule {
        langs: &["nix"],
        separator: '/',
        extensions: &["", ".nix"],
        index_files: &["default.nix"],
        roots: &["", "nix", "modules", "pkgs"],
    },
    // Pascal. A unit is one file named for it, but Delphi allows both
    // `App.Helpers.pas` and `App/Helpers.pas` for a dotted unit name, so the
    // literal form is tried first and the separator form second — which the
    // generator does for every dotted rule.
    ImportPathRule {
        langs: &["pascal"],
        separator: '.',
        extensions: &[".pas", ".pp", ".dpr"],
        index_files: &[],
        roots: &["", "src", "units", "source"],
    },
    ImportPathRule {
        langs: &["solidity"],
        separator: '/',
        extensions: &["", ".sol"],
        index_files: &[],
        roots: &["", "contracts", "src", "lib", "node_modules"],
    },
    // Erlang. `-include("records.hrl")` is resolved against the including
    // file's directory and the `include` directory of the application, which
    // is where `rebar3` and `erlc -I` both point.
    ImportPathRule {
        langs: &["erlang"],
        separator: '/',
        extensions: &["", ".hrl", ".erl"],
        index_files: &[],
        roots: &["", "include", "src", "apps", "lib"],
    },
    // CFML. `template="header.cfm"` is resolved against the web root, which
    // for a repository is the root or a views directory.
    ImportPathRule {
        langs: &["cfml"],
        separator: '/',
        extensions: &["", ".cfm", ".cfc"],
        index_files: &[],
        roots: &["", "views", "src", "components", "includes"],
    },
];

/// The rule serving `lang`, if any.
pub fn rule_for(lang: &str) -> Option<&'static ImportPathRule> {
    IMPORT_PATH_RULES
        .iter()
        .find(|rule| rule.langs.contains(&lang))
}

/// Join a relative specifier onto a base directory and resolve `.` and `..`.
/// Absolute paths and parents above the repository return `None`: clamping an
/// escaping path to the root would bind an unrelated, same-named local file.
///
/// The single owner of this normalisation. `Resolver::normalize_rel` delegates
/// here rather than keeping a second copy, because a `..` handled two ways is
/// how a resolver starts answering two different questions about one import.
pub fn normalize_rel(base_dir: &str, spec: &str) -> Option<String> {
    let absolute_or_drive = |path: &str| {
        path.starts_with(['/', '\\'])
            || (path.as_bytes().get(1) == Some(&b':')
                && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
    };
    if absolute_or_drive(base_dir) || absolute_or_drive(spec) {
        return None;
    }
    let joined = if base_dir.is_empty() || base_dir == "." {
        spec.to_string()
    } else {
        format!("{base_dir}/{spec}")
    };
    let norm = joined.replace('\\', "/");
    let mut stack: Vec<&str> = Vec::new();
    for part in norm.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            other => stack.push(other),
        }
    }
    Some(stack.join("/"))
}

/// How many candidate paths one specifier may produce.
///
/// The generator is a product of roots × extensions × forms, so it is bounded
/// by construction — the largest row yields well under this. The cap is here so
/// that a row added later with a long roots list cannot turn one import into an
/// unbounded probe, and it is an assertion about the *table*, not about input:
/// nothing a repository contains can grow this list.
pub const MAX_CANDIDATES_PER_SPECIFIER: usize = 96;

/// Candidate repository paths for one import, most specific first.
///
/// Order is the whole contract. The importing file's own directory comes first
/// because every one of these languages resolves a relative specifier against
/// it, and a root-relative candidate that also matches would otherwise win and
/// point the edge at the wrong file of the same name.
pub fn candidates(rule: &ImportPathRule, importing_dir: &str, specifier: &str) -> Vec<String> {
    let spec = specifier.trim().trim_matches(|c| c == '"' || c == '\'');
    if spec.is_empty() {
        return Vec::new();
    }

    // Forms the specifier can take as a path. A dotted or backslashed
    // specifier yields two: the literal text (Delphi's `App.Helpers.pas`, a
    // Windows-style PHP path) and the separator-substituted one.
    let mut forms: Vec<String> = Vec::new();
    let substituted = spec.replace(rule.separator, "/");
    if rule.separator != '/' {
        forms.push(spec.to_string());
    }
    if !forms.contains(&substituted) {
        forms.push(substituted);
    }

    // A specifier written relative is *only* relative: resolving `./util.h`
    // against the repository root as well would let a same-named file
    // elsewhere claim the edge.
    let roots: Vec<&str> = if is_relative_specifier(spec) {
        vec![]
    } else {
        rule.roots.to_vec()
    };

    let mut out: Vec<String> = Vec::new();
    let push = |path: String, out: &mut Vec<String>| {
        if !path.is_empty() && !out.contains(&path) && out.len() < MAX_CANDIDATES_PER_SPECIFIER {
            out.push(path);
        }
    };

    for form in &forms {
        // The importing file's own directory, first and always.
        if let Some(base) = normalize_rel(importing_dir, form) {
            for extension in rule.extensions {
                push(format!("{base}{extension}"), &mut out);
            }
            for index in rule.index_files {
                push(format!("{base}/{index}"), &mut out);
            }
        }
        // Then the language's build roots, for a specifier that is not written
        // relative.
        for root in &roots {
            let Some(rooted) = normalize_rel(root, form) else {
                continue;
            };
            for extension in rule.extensions {
                push(format!("{rooted}{extension}"), &mut out);
            }
            for index in rule.index_files {
                push(format!("{rooted}/{index}"), &mut out);
            }
        }
    }
    out
}

/// Strip a Dart `package:<name>/` prefix, which names a pubspec package rather
/// than a directory. The remainder is `lib`-relative, which the Dart row's
/// roots already carry.
pub fn strip_dart_package_prefix(specifier: &str) -> Option<&str> {
    specifier
        .strip_prefix("package:")
        .and_then(|rest| rest.split_once('/'))
        .map(|(_package, path)| path)
}

/// Whether the author wrote this specifier as a path relative to their own
/// file.
///
/// Load-bearing twice over: it removes the build roots from the candidate
/// ladder, and it removes the basename rung entirely. `./util.h` is a precise
/// statement of where the file is, so a `util.h` elsewhere in the repository is
/// **not** the answer — measured: without this, `#include "./util.h"` in
/// `src/deep/main.c` produced an edge to a root-level `util.h` the author never
/// referred to. An unresolved relative import is recorded as such, which is a
/// missing edge; a wrong one is worse.
pub fn is_relative_specifier(specifier: &str) -> bool {
    let spec = specifier.trim().trim_matches(|c| c == '"' || c == '\'');
    spec.starts_with("./")
        || spec.starts_with("../")
        || spec.starts_with(".\\")
        || spec.starts_with("..\\")
}
