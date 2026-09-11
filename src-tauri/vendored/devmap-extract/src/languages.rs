use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExtractorId {
    TypeScript,
    Tsx,
    JavaScript,
    ArkTs,
    Python,
    Go,
    Rust,
    Java,
    CSharp,
    VbNet,
    Php,
    Ruby,
    C,
    Cpp,
    ObjC,
    Metal,
    Cuda,
    Swift,
    Kotlin,
    Scala,
    Dart,
    Svelte,
    Vue,
    Astro,
    Liquid,
    Pascal,
    Lua,
    Luau,
    R,
    Cfml,
    Cobol,
    Erlang,
    Solidity,
    Terraform,
    Nix,
    GenericTreeSitter,
}

impl ExtractorId {
    pub fn name(&self) -> &'static str {
        match self {
            ExtractorId::TypeScript => "typescript",
            ExtractorId::Tsx => "tsx",
            ExtractorId::JavaScript => "javascript",
            ExtractorId::ArkTs => "arkts",
            ExtractorId::Python => "python",
            ExtractorId::Go => "go",
            ExtractorId::Rust => "rust",
            ExtractorId::Java => "java",
            ExtractorId::CSharp => "csharp",
            ExtractorId::VbNet => "vbnet",
            ExtractorId::Php => "php",
            ExtractorId::Ruby => "ruby",
            ExtractorId::C => "c",
            ExtractorId::Cpp => "cpp",
            ExtractorId::ObjC => "objc",
            ExtractorId::Metal => "metal",
            ExtractorId::Cuda => "cuda",
            ExtractorId::Swift => "swift",
            ExtractorId::Kotlin => "kotlin",
            ExtractorId::Scala => "scala",
            ExtractorId::Dart => "dart",
            ExtractorId::Svelte => "svelte",
            ExtractorId::Vue => "vue",
            ExtractorId::Astro => "astro",
            ExtractorId::Liquid => "liquid",
            ExtractorId::Pascal => "pascal",
            ExtractorId::Lua => "lua",
            ExtractorId::Luau => "luau",
            ExtractorId::R => "r",
            ExtractorId::Cfml => "cfml",
            ExtractorId::Cobol => "cobol",
            ExtractorId::Erlang => "erlang",
            ExtractorId::Solidity => "solidity",
            ExtractorId::Terraform => "terraform",
            ExtractorId::Nix => "nix",
            ExtractorId::GenericTreeSitter => "generic",
        }
    }
}

/// One thing the extractor is able to observe in a language.
///
/// The discriminants are bit positions in [`Capabilities`]; they are part of no
/// serialized format and may be renumbered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Capability {
    /// The extractor produces `Extraction::calls` — invocations with a callee
    /// name. Without it, "nothing calls this symbol" is a statement about the
    /// extractor, not about the code.
    Calls = 1,
    /// The extractor produces `Extraction::imports` — module specifiers that
    /// become `EdgeKind::Imports`. Without it, "nothing imports this file" is
    /// likewise vacuous, which is what `unwired_candidates` reads.
    Imports = 2,
    /// The extractor produces `Extraction::references` — non-call uses.
    References = 4,
    /// The extractor produces `ReferenceKind::Heritage` — a supertype named in
    /// a declaration, which is what an `Extends`/`Implements` edge needs.
    Heritage = 8,
}

impl Capability {
    /// Every capability, in bit order. The single list the derivation test
    /// iterates, so a new variant cannot be added without the test covering it.
    pub const ALL: &'static [Capability] = &[
        Capability::Calls,
        Capability::Imports,
        Capability::References,
        Capability::Heritage,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Capability::Calls => "calls",
            Capability::Imports => "imports",
            Capability::References => "references",
            Capability::Heritage => "heritage",
        }
    }
}

/// What a language's extractor demonstrably produces.
///
/// A *derived* fact, not a remembered one. `CALL_EXTRACTION_LANGUAGES` — the
/// hand-written list this replaces — claimed in its own doc comment to be read
/// by a coverage report that did not exist, had zero production readers, and
/// was wrong by omission for 14 languages. The failure mode was structural: a
/// list nothing checks against behaviour rots the moment behaviour moves.
///
/// So the contract here is that every bit is pinned, in **both directions**,
/// against an observation over `testdata/capabilities/` — see
/// `tests/language_capabilities.rs`. A language that gains a `langcalls`
/// module and forgets the flag fails CI; so does a flag set for a language
/// that produces nothing.
///
/// Keyed on the **grammar**, not the language name: `detect_language` returns
/// `spec.grammar`, every dispatch site matches on it, and `Extraction::language`
/// stores it. ArkTS and TypeScript share `typescript`; Metal and C++ share
/// `cpp`. Two specs naming one grammar therefore must declare identical
/// capabilities, which `capabilities_agree_within_a_grammar` asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
pub struct Capabilities(u8);

impl Capabilities {
    /// The extractor observes nothing for this language.
    ///
    /// Not the same as "this language has nothing to observe" — it is the
    /// fail-closed default, and the value that makes a blind spot say so.
    pub const NONE: Capabilities = Capabilities(0);

    pub const fn new(bits: u8) -> Capabilities {
        Capabilities(bits)
    }

    pub const fn contains(self, capability: Capability) -> bool {
        self.0 & (capability as u8) != 0
    }

    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// The capabilities this language has, in bit order. Used to report a
    /// blind spot by name rather than as a bare boolean.
    pub fn iter(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .iter()
            .copied()
            .filter(move |cap| self.contains(*cap))
    }

    /// The capabilities this build lacks for the language, in bit order.
    ///
    /// The complement of [`Self::iter`], and the projection a *disclosure*
    /// wants: a reader deciding whether an empty answer is a fact or a hole
    /// needs the bits that are clear, not the ones that are set.
    pub fn missing(self) -> impl Iterator<Item = Capability> {
        Capability::ALL
            .iter()
            .copied()
            .filter(move |cap| !self.contains(*cap))
    }

    /// Every capability either side observes.
    ///
    /// Needed because one language *key* can cover extractions with different
    /// answers: `notebook` resolves per file through
    /// [`crate::model::Extraction::capabilities`], so a corpus with a Python
    /// notebook and an R notebook has one row and two capability sets behind it.
    /// The union is the honest aggregate — the row means "this build can observe
    /// X for files reported under this key", and it can, for some of them.
    pub const fn union(self, other: Capabilities) -> Capabilities {
        Capabilities(self.0 | other.0)
    }
}

/// `Capabilities::new(CALLS | REFERENCES)` — bare `u8` constants so the set
/// composes with `|` inside a `static` initializer, which a `BitOr` impl
/// cannot do on stable.
pub const CALLS: u8 = Capability::Calls as u8;
pub const IMPORTS: u8 = Capability::Imports as u8;
pub const REFERENCES: u8 = Capability::References as u8;
pub const HERITAGE: u8 = Capability::Heritage as u8;

/// What a file-level liveness question is asked *about* in this language.
///
/// "Nothing imports this file" is a finding in Python and a category error in
/// Markdown, and until this column existed the difference was decided by
/// [`crate::model::Extraction::grammar_read_this_file`] — a question about
/// which engine ran. That worked only by coincidence: prose is excluded from
/// `unwired_candidates` because no grammar reads it, so the day a YAML or JSON
/// grammar is linked, every `.md`-adjacent data file in every repository
/// becomes a delete-this suggestion again. Which engine ran and whether the
/// file can be stranded are different questions, and this is the second one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LivenessUnit {
    /// One file is one module: another file can name it, so "nothing names it"
    /// is a question with an answer.
    Module,
    /// The directory is the unit the toolchain uses, and **this build resolves
    /// nothing at that level**. No statement an author could write names one
    /// `.tf` file, and no synthetic directory node stands in for it, so a
    /// per-file verdict is vacuous however the graph turns out.
    ///
    /// Go is deliberately *not* here even though a Go package is a directory,
    /// and the difference is that the resolver does emit a
    /// `package:<dir>/<pkg>` node for it: `unwired_candidates` reads that node
    /// back, so a file in an imported package is cleared and a package nothing
    /// imports is still reported. Marking Go `Directory` would blanket-exempt
    /// every Go file and delete that second finding, which
    /// `a_go_package_nothing_imports_is_still_a_candidate` refuses.
    Directory,
    /// Prose, data or configuration. It declares nothing that could be
    /// stranded, and nothing imports it because there is nothing to import.
    Data,
}

impl LivenessUnit {
    /// The wording a `NotCode` verdict carries for this unit.
    ///
    /// Only `Data` has one — the other two are not exclusions — so this
    /// returns `None` rather than a placeholder sentence for them.
    pub fn not_code_reason(self) -> Option<&'static str> {
        match self {
            LivenessUnit::Data => {
                Some("prose, data or configuration: it declares nothing that could be stranded")
            }
            LivenessUnit::Module | LivenessUnit::Directory => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanguageSpec {
    pub name: &'static str,
    pub grammar: &'static str,
    pub extensions: &'static [&'static str],
    pub embedded: &'static [&'static str],
    pub extractor_id: ExtractorId,
    pub lsp_id: &'static str,
    pub viz_color: &'static str,
    /// What this language's extractor produces. See [`Capabilities`].
    ///
    /// Not compared against `testdata/golden/language_specs.json`: that frozen
    /// manifest records what *Python* declared, and Python declared nothing
    /// about capability. `FrozenLanguageSpec` names its five fields explicitly
    /// and the parity test compares field by field, so this one is invisible
    /// to it by construction rather than by an exemption.
    pub capabilities: Capabilities,
    /// What a file-level liveness verdict means here. See [`LivenessUnit`].
    ///
    /// Invisible to the frozen-registry parity test for the same reason
    /// `capabilities` is: Python declared no such thing.
    pub liveness_unit: LivenessUnit,
}

pub static LANGUAGE_SPECS: &[LanguageSpec] = &[
    LanguageSpec {
        name: "TypeScript",
        grammar: "typescript",
        extensions: &[".ts", ".mts", ".cts"],
        embedded: &[],
        extractor_id: ExtractorId::TypeScript,
        lsp_id: "typescript",
        viz_color: "#3178c6",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "TSX",
        grammar: "tsx",
        extensions: &[".tsx"],
        embedded: &[],
        extractor_id: ExtractorId::Tsx,
        lsp_id: "typescript",
        viz_color: "#3178c6",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "JavaScript",
        grammar: "javascript",
        extensions: &[".js", ".jsx", ".mjs", ".cjs"],
        embedded: &[],
        extractor_id: ExtractorId::JavaScript,
        lsp_id: "javascript",
        viz_color: "#f7df1e",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "ArkTS",
        grammar: "typescript",
        extensions: &[".ets"],
        embedded: &[],
        extractor_id: ExtractorId::ArkTs,
        lsp_id: "typescript",
        viz_color: "#002b36",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Python",
        grammar: "python",
        extensions: &[".py", ".pyi"],
        embedded: &[],
        extractor_id: ExtractorId::Python,
        lsp_id: "python",
        viz_color: "#3572A5",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Go",
        grammar: "go",
        extensions: &[".go"],
        embedded: &[],
        extractor_id: ExtractorId::Go,
        lsp_id: "gopls",
        viz_color: "#00ADD8",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Rust",
        grammar: "rust",
        extensions: &[".rs"],
        embedded: &[],
        extractor_id: ExtractorId::Rust,
        lsp_id: "rust-analyzer",
        viz_color: "#dea584",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Java",
        grammar: "java",
        extensions: &[".java"],
        embedded: &[],
        extractor_id: ExtractorId::Java,
        lsp_id: "jdtls",
        viz_color: "#b07219",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "C#",
        grammar: "csharp",
        extensions: &[".cs"],
        embedded: &[],
        extractor_id: ExtractorId::CSharp,
        lsp_id: "omnisharp",
        viz_color: "#178600",
        capabilities: Capabilities::new(CALLS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "VB.NET",
        grammar: "vb",
        extensions: &[".vb"],
        embedded: &[],
        extractor_id: ExtractorId::VbNet,
        lsp_id: "vbnet",
        viz_color: "#945db7",
        capabilities: Capabilities::NONE,
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "PHP",
        grammar: "php",
        extensions: &[".php", ".phtml"],
        embedded: &[],
        extractor_id: ExtractorId::Php,
        lsp_id: "intelephense",
        viz_color: "#4F5D95",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Ruby",
        grammar: "ruby",
        extensions: &[".rb", ".rake"],
        embedded: &[],
        extractor_id: ExtractorId::Ruby,
        lsp_id: "solargraph",
        viz_color: "#701516",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "C",
        grammar: "c",
        extensions: &[".c", ".h"],
        embedded: &[],
        extractor_id: ExtractorId::C,
        lsp_id: "clangd",
        viz_color: "#555555",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "C++",
        grammar: "cpp",
        extensions: &[".cc", ".cpp", ".cxx", ".hh", ".hpp", ".hxx"],
        embedded: &[],
        extractor_id: ExtractorId::Cpp,
        lsp_id: "clangd",
        viz_color: "#f34b7d",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Objective-C",
        grammar: "objc",
        extensions: &[".m", ".mm"],
        embedded: &[],
        extractor_id: ExtractorId::ObjC,
        lsp_id: "clangd",
        viz_color: "#438eff",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Metal",
        grammar: "cpp",
        extensions: &[".metal"],
        embedded: &[],
        extractor_id: ExtractorId::Metal,
        lsp_id: "clangd",
        viz_color: "#8f14e9",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "CUDA",
        grammar: "cuda",
        extensions: &[".cu", ".cuh"],
        embedded: &[],
        extractor_id: ExtractorId::Cuda,
        lsp_id: "clangd",
        viz_color: "#3A4E3A",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Swift",
        grammar: "swift",
        extensions: &[".swift"],
        embedded: &[],
        extractor_id: ExtractorId::Swift,
        lsp_id: "sourcekit-lsp",
        viz_color: "#F05138",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Kotlin",
        grammar: "kotlin",
        extensions: &[".kt", ".kts"],
        embedded: &[],
        extractor_id: ExtractorId::Kotlin,
        lsp_id: "kotlin-language-server",
        viz_color: "#A97BFF",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Scala",
        grammar: "scala",
        extensions: &[".scala", ".sc"],
        embedded: &[],
        extractor_id: ExtractorId::Scala,
        lsp_id: "metals",
        viz_color: "#c22d40",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Dart",
        grammar: "dart",
        extensions: &[".dart"],
        embedded: &[],
        extractor_id: ExtractorId::Dart,
        lsp_id: "dart-analysis-server",
        viz_color: "#00B4AB",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Svelte",
        grammar: "svelte",
        extensions: &[".svelte"],
        embedded: &["typescript", "javascript", "css", "html"],
        extractor_id: ExtractorId::Svelte,
        lsp_id: "svelte-language-server",
        viz_color: "#ff3e00",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Vue",
        grammar: "vue",
        extensions: &[".vue"],
        // `tsx` is Vue's alone among the four template languages here. Vue
        // single-file components are routinely written with JSX render
        // functions and its own compiler accepts `<script lang="tsx">`.
        // Svelte's template is not JSX, and an Astro `<script>` is plain
        // JS/TS — Astro components that use JSX are `.jsx`/`.tsx` files the
        // registry already claims by extension. Permitting `tsx` there would
        // parse a region under a grammar its framework never compiles it with.
        // `jsx` needs no entry: tree-sitter-javascript parses JSX already.
        embedded: &["typescript", "tsx", "javascript", "css", "html"],
        extractor_id: ExtractorId::Vue,
        lsp_id: "volar",
        viz_color: "#41b883",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Astro",
        grammar: "astro",
        extensions: &[".astro"],
        embedded: &["typescript", "javascript", "css", "html"],
        extractor_id: ExtractorId::Astro,
        lsp_id: "astro-ls",
        viz_color: "#ff5a03",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Liquid",
        grammar: "liquid",
        extensions: &[".liquid"],
        embedded: &["html", "javascript", "css"],
        extractor_id: ExtractorId::Liquid,
        lsp_id: "theme-check",
        viz_color: "#67b8de",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Pascal/Delphi",
        grammar: "pascal",
        extensions: &[".pas", ".pp", ".dpr"],
        embedded: &[],
        extractor_id: ExtractorId::Pascal,
        lsp_id: "pascal-lsp",
        viz_color: "#E3F171",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Lua",
        grammar: "lua",
        extensions: &[".lua"],
        embedded: &[],
        extractor_id: ExtractorId::Lua,
        lsp_id: "lua-language-server",
        viz_color: "#000080",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Luau",
        grammar: "luau",
        extensions: &[".luau"],
        embedded: &[],
        extractor_id: ExtractorId::Luau,
        lsp_id: "luau-lsp",
        viz_color: "#00A2FF",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "R",
        grammar: "r",
        extensions: &[".r", ".R"],
        embedded: &[],
        extractor_id: ExtractorId::R,
        lsp_id: "r-languageserver",
        viz_color: "#198CE7",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "CFML",
        grammar: "cfml",
        extensions: &[".cfm", ".cfc"],
        embedded: &[],
        extractor_id: ExtractorId::Cfml,
        lsp_id: "cfls",
        viz_color: "#224f80",
        capabilities: Capabilities::new(IMPORTS),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "COBOL",
        grammar: "cobol",
        extensions: &[".cob", ".cbl", ".cpy"],
        embedded: &[],
        extractor_id: ExtractorId::Cobol,
        lsp_id: "cobol-ls",
        viz_color: "#005ca5",
        capabilities: Capabilities::NONE,
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Erlang",
        grammar: "erlang",
        extensions: &[".erl", ".hrl"],
        embedded: &[],
        extractor_id: ExtractorId::Erlang,
        lsp_id: "erlang-ls",
        viz_color: "#B83998",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Solidity",
        grammar: "solidity",
        extensions: &[".sol"],
        embedded: &[],
        extractor_id: ExtractorId::Solidity,
        lsp_id: "solc",
        viz_color: "#AA6746",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES | HERITAGE),
        liveness_unit: LivenessUnit::Module,
    },
    LanguageSpec {
        name: "Terraform/OpenTofu",
        grammar: "hcl",
        extensions: &[".tf", ".tfvars", ".hcl"],
        embedded: &[],
        extractor_id: ExtractorId::Terraform,
        lsp_id: "terraform-ls",
        viz_color: "#5C4EE5",
        capabilities: Capabilities::new(IMPORTS | REFERENCES),
        // A Terraform *directory* is the module. `terraform apply` reads every
        // `.tf` in one folder as one body, and there is no statement an author
        // could write that names a single file in it — so "nothing imports
        // `main.tf`" is true of every well-formed Terraform repository and is
        // never a finding.
        liveness_unit: LivenessUnit::Directory,
    },
    LanguageSpec {
        name: "Nix",
        grammar: "nix",
        extensions: &[".nix"],
        embedded: &[],
        extractor_id: ExtractorId::Nix,
        lsp_id: "nil",
        viz_color: "#7e71de",
        capabilities: Capabilities::new(CALLS | IMPORTS | REFERENCES),
        liveness_unit: LivenessUnit::Module,
    },
];

pub fn find_spec_by_extension(ext: &str) -> Option<&'static LanguageSpec> {
    let ext_lower = if ext.starts_with('.') {
        ext.to_lowercase()
    } else {
        format!(".{}", ext.to_lowercase())
    };
    LANGUAGE_SPECS.iter().find(|spec| {
        spec.extensions
            .iter()
            .any(|&e| e.to_lowercase() == ext_lower)
    })
}

/// Extractor identity for a path, or `None` when no spec claims the extension.
///
/// `detect_language` answers "which grammar parses this", which is deliberately
/// many-to-one: Metal and C++ both answer `"cpp"`, ArkTS and TypeScript both
/// answer `"typescript"`. That is the right key for the parser and the grammar
/// version, and the wrong one for any rule that is true of the *language* and
/// false of the grammar it borrows. Metal is the live case — `kernel`,
/// `device` and `constant` are declaration qualifiers there and syntax errors
/// in C++ — so the distinction is read from the frozen registry rather than
/// re-derived from a second extension list that could drift out of step with it.
pub fn detect_extractor_id(path: &Path) -> Option<ExtractorId> {
    let ext = path.extension().and_then(|s| s.to_str())?;
    find_spec_by_extension(ext).map(|spec| spec.extractor_id)
}

/// Language id for a path, or `"generic"` when unknown.
pub fn detect_language(path: &Path) -> &'static str {
    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if filename == "pyproject.toml"
        || filename == "Cargo.toml"
        || filename == "package.json"
        || filename == "go.mod"
        || filename == "jsconfig.json"
        || filename == "tsconfig.json"
    {
        return "config";
    }

    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if let Some(spec) = find_spec_by_extension(ext) {
        spec.grammar
    } else {
        match ext {
            "sh" | "bash" | "zsh" => "shell",
            // Named here rather than left as "generic" so `is_indexable_source`
            // admits them at all. Both are source with real declarations and no
            // linked grammar, so discovery was dropping them before extraction
            // ever ran: 19 `.proto` and 17 `.ps1` files on one corpus that the
            // graph could not see. Tier-2 recovery (`crate::fallback`) reads
            // protobuf `message`/`service`/`rpc` and PowerShell `function`.
            "proto" => "protobuf",
            "ps1" | "psm1" | "psd1" => "powershell",
            // A notebook is source, and `crate::notebook` knows how to read it
            // (G5). Named here for the same reason as the two above: discovery
            // admits only what `detect_language` names, so an unnamed extension
            // never reaches an extractor that could handle it.
            "ipynb" => "notebook",
            "html" | "htm" => "html",
            "css" | "scss" | "less" => "css",
            "sql" => "sql",
            "yaml" | "yml" => "yaml",
            "json" => "json",
            "toml" => "toml",
            "md" | "markdown" => "markdown",
            _ => "generic",
        }
    }
}

/// Grammars reachable through `detect_language`'s fallback table rather than
/// through `LANGUAGE_SPECS`.
///
/// `find_spec_by_extension` misses these entirely — `.sh` and `.sql` are named
/// only in the `match ext` arm below `detect_language`'s registry lookup — yet
/// both reach a real grammar and both extract calls (`langcalls::shell`,
/// `langcalls::sql`). A capability lookup that consulted only the registry
/// would report them blind and charge working extraction as a coverage hole,
/// which is the same class of error as the list this replaces.
///
/// The prose and data formats are listed at `NONE` deliberately rather than
/// left to the fallback: an explicit row is a decision, an absence is an
/// oversight, and `every_reachable_grammar_declares_capabilities` cannot tell
/// them apart otherwise.
/// Capability rows for the grammars `detect_language` reaches through its
/// fallback table rather than through [`LANGUAGE_SPECS`].
///
/// Public so `language_capabilities.rs` can require a probe for each, the way it
/// already does for the registry. It could not before, and the notebook row was
/// wrong for as long as nothing looked at it.
/// One row carries both facts on purpose. A second table keyed on the same
/// grammar names is a second place to forget a row, and the two would then
/// disagree about a language rather than about a field —
/// `every_reachable_grammar_declares_capabilities` can only catch the one it
/// iterates.
pub const NON_REGISTRY_CAPABILITIES: &[(&str, Capabilities, LivenessUnit)] = &[
    (
        "shell",
        Capabilities::new(CALLS | IMPORTS | REFERENCES),
        LivenessUnit::Module,
    ),
    // A `.sql` file is code: a grammar reads it, it declares views and
    // procedures, and a migration runner or an application names it. It has no
    // *import* extractor, which is a capability gap and is charged as one —
    // deliberately not folded in here, because "we cannot see imports in SQL"
    // and "SQL cannot be stranded" are different claims with different
    // remedies, and `only_source_files_are_charged_as_import_blind` pins the
    // first.
    (
        "sql",
        Capabilities::new(CALLS | REFERENCES),
        LivenessUnit::Module,
    ),
    // No linked grammar. Both reach `crate::fallback`, which recovers
    // declarations by line pattern and by construction extracts nothing else —
    // already charged to coverage as `PatternRecovered`.
    ("protobuf", Capabilities::NONE, LivenessUnit::Module),
    ("powershell", Capabilities::NONE, LivenessUnit::Module),
    // Prose, data and config. No grammar is wanted and none will come; these
    // report `ExtractionEngine::NotApplicable` and are excluded from coverage
    // by `Extraction::is_parse_failure`.
    //
    // `LivenessUnit::Data` is what keeps them out of `unwired_candidates`, and
    // it is a claim about the *format* rather than about which engine ran. The
    // exclusion used to ride on `grammar_read_this_file()`, so linking a YAML
    // or JSON grammar would have re-created the historical bug in which every
    // `.md`, `.json` and `.yaml` in every repository was a delete-this
    // suggestion.
    ("markdown", Capabilities::NONE, LivenessUnit::Data),
    ("json", Capabilities::NONE, LivenessUnit::Data),
    ("yaml", Capabilities::NONE, LivenessUnit::Data),
    ("toml", Capabilities::NONE, LivenessUnit::Data),
    ("html", Capabilities::NONE, LivenessUnit::Data),
    ("css", Capabilities::NONE, LivenessUnit::Data),
    ("config", Capabilities::NONE, LivenessUnit::Data),
    ("generic", Capabilities::NONE, LivenessUnit::Data),
    // A notebook is re-parsed with its kernel's grammar, so its capabilities
    // are that grammar's, resolved per file rather than declared here.
    //
    // "Resolved per file" was an intention with no implementation for as long as
    // this row existed. `capabilities_for_language` takes a `&str` and
    // `Extraction::language` for an `.ipynb` stays `"notebook"` — the kernel
    // name goes into `ExtractionEngine::Notebook { kernel_language }` and never
    // into `language` — so every clean notebook took this `NONE` verbatim: it
    // was charged both `CallBlind` and `ImportBlind` with the false reason
    // "`notebook` has no call extractor in this build", dropped from
    // `files_with_call_extraction`, made `file_is_call_blind` for every symbol
    // it declares, and counted into `unwired_candidates`' import-blind
    // exclusions — while `notebook.rs` was filling `extraction.imports` and
    // `extraction.calls` and `grammar_read_this_file()` returned true.
    //
    // [`crate::model::Extraction::capabilities`] is the resolver this comment
    // always described, and every production charge site asks it. This row is
    // the fail-closed answer for a caller holding only the string.
    ("notebook", Capabilities::NONE, LivenessUnit::Module),
];

/// What the extractor can observe in `language`, where `language` is the
/// **grammar** string `detect_language` returns and `Extraction::language`
/// stores.
///
/// The canonical owner. Every consumer asking "did we even look for calls in
/// this file" asks here, so a blind spot and a genuine negative cannot become
/// indistinguishable in one caller while staying distinct in another.
///
/// An unrecognised language is [`Capabilities::NONE`] — fail closed, so a new
/// grammar wired up without a capability row reports as blind rather than
/// silently inheriting a confident verdict it has not earned.
pub fn capabilities_for_language(language: &str) -> Capabilities {
    if let Some(spec) = LANGUAGE_SPECS.iter().find(|s| s.grammar == language) {
        return spec.capabilities;
    }
    NON_REGISTRY_CAPABILITIES
        .iter()
        .find(|(name, _, _)| *name == language)
        .map(|(_, caps, _)| *caps)
        .unwrap_or(Capabilities::NONE)
}

/// What a file-level liveness verdict means for `language`, where `language`
/// is the **grammar** string `detect_language` returns.
///
/// The canonical owner, for the same reason [`capabilities_for_language`] is
/// one: `unwired_candidates`, `files_wholly_inside_clusters` and
/// `analyze_liveness` each used to decide separately which files could be
/// called dead, and three copies of a rule are three chances to disagree about
/// a README.
///
/// An unrecognised language is [`LivenessUnit::Module`], which is fail-*open*
/// for this rule and deliberately so: the cost of a wrong `Module` is one more
/// finding a reader can dismiss, and the cost of a wrong `Data` is a finding
/// that is never shown. `wiring::config_entry_point_symbols` takes the same
/// direction for the same reason.
pub fn liveness_unit_for_language(language: &str) -> LivenessUnit {
    if let Some(spec) = LANGUAGE_SPECS.iter().find(|s| s.grammar == language) {
        return spec.liveness_unit;
    }
    NON_REGISTRY_CAPABILITIES
        .iter()
        .find(|(name, _, _)| *name == language)
        .map(|(_, _, unit)| *unit)
        .unwrap_or(LivenessUnit::Module)
}

/// Why this path is data whatever grammar reads it, or `None`.
///
/// The language table answers for a *format*; this answers for the handful of
/// paths whose format lies about them. `infra/.terraform.lock.hcl` parses as
/// HCL and declares `provider` blocks, so every gate that asks the engine says
/// "a grammar read this" and reports a lockfile as a stranded module. The same
/// shape covers `package-lock.json` — excluded today only because no JSON
/// grammar is linked — and every `.env`, which is credentials rather than
/// code.
///
/// Matched on the **basename**, lowercased, so `Config/.ENV.Production` is the
/// same file as `config/.env.production`. Directory position is not consulted:
/// a lockfile is a lockfile wherever it is checked in.
///
/// Deliberately narrow. Each arm names a file kind whose whole purpose is to
/// be machine-written or machine-read, because the verdict it produces —
/// `FileLiveness::NotCode` — takes the path out of every liveness answer at
/// once. A suffix that is merely *suggestive* of data would hide real findings.
pub fn non_code_path_reason(path: &str) -> Option<&'static str> {
    let normalized = path.replace('\\', "/");
    let name = normalized
        .rsplit('/')
        .next()
        .unwrap_or(&normalized)
        .to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    // `.env`, `.env.production`, `.envrc` — and `prod.env` / `x.env`, which is
    // the same file under the other naming convention. A file merely *named*
    // `env` is not one: without the dot there is no delimiter, and
    // `environment.ts` must stay code.
    if name.starts_with(".env") || name.ends_with(".env") {
        return Some("an environment file: values a process reads, never code");
    }
    // `Cargo.lock`, `flake.lock`, `poetry.lock`, `.terraform.lock.hcl`,
    // `package-lock.json`. The `-lock.<ext>` spelling is npm's and pnpm's; the
    // `.lock.<ext>` one is Terraform's.
    if name.ends_with(".lock")
        || name.contains(".lock.")
        || name.contains("-lock.")
        || name == "gemfile.lock"
    {
        return Some("a lockfile: resolved dependency versions written by a tool");
    }
    // Terraform variable *values*. The `.tf` files in the directory read them;
    // nothing imports one, and unlike a `.tf` file it declares no resources at
    // all, so it is data rather than a directory unit.
    if name.ends_with(".tfvars") || name.ends_with(".tfvars.json") {
        return Some("Terraform variable values: data the directory's modules read");
    }
    None
}

/// Whether `language` is one this build has a capability row for at all.
///
/// Distinct from `capabilities_for_language(..).is_none()`, which cannot tell
/// "declared to observe nothing" from "never heard of it". Only the matrix
/// test needs the difference, and it needs it precisely so an unrowed grammar
/// fails loudly instead of defaulting quietly.
pub fn language_capability_is_declared(language: &str) -> bool {
    LANGUAGE_SPECS.iter().any(|s| s.grammar == language)
        || NON_REGISTRY_CAPABILITIES
            .iter()
            .any(|(name, _, _)| *name == language)
}

/// Every language id this build declares, deduplicated and sorted.
///
/// The enumerable form of [`language_capability_is_declared`], and the exact
/// set `detect_language` can return: the registry's grammars plus the fallback
/// table's rows. A consumer that has to hold a row *per language* — the map
/// visualizer's colour table is the first — needs the list itself, not a
/// membership test, or its own coverage check degrades into "the ids I
/// remembered to think of".
pub fn declared_language_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = LANGUAGE_SPECS
        .iter()
        .map(|spec| spec.grammar)
        .chain(NON_REGISTRY_CAPABILITIES.iter().map(|(name, _, _)| *name))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Whether a relative path is inside a VCS, build, environment, or devmap-owned
/// namespace. This is the shared admission boundary for cold walks and live
/// watcher events.
///
/// Also applies [`is_default_index_excluded`] — fixtures under `testdata/` and
/// vendored grammar C under `vendor/grammars/` are not program text this index
/// should charge as coverage loss. Override or extend via
/// [`INDEX_EXCLUDES_ENV`].
pub fn is_ignored_path(rel_path: &str) -> bool {
    let norm = rel_path.replace('\\', "/");
    if norm.is_empty() {
        return true;
    }
    if norm.starts_with('.') && !norm.starts_with("./") {
        // Skip VCS / tooling dirs at root of walk (`.git`, `.devcouncil`, …)
        let first = norm.split('/').next().unwrap_or("");
        if first.starts_with('.') && first != "." {
            return true;
        }
    }
    let lower = norm.to_lowercase();
    for part in lower.split('/') {
        // Both state directory names, always — a repository mid-migration has
        // `.devmap/` and `.devcouncil/` on disk at once, and indexing either
        // would put a sqlite store and a 20 MB graph into the graph.
        if crate::paths::is_state_dir_name(part) {
            return true;
        }
        if matches!(
            part,
            "target"
                | "node_modules"
                | ".git"
                | "dist"
                | "build"
                | "__pycache__"
                | ".venv"
                | "venv"
                | ".tox"
                | "coverage"
                | ".idea"
                | ".vscode"
        ) {
            return true;
        }
    }
    if is_default_index_excluded(&norm) {
        return true;
    }
    false
}

/// Environment variable naming extra path prefixes to exclude from discovery.
///
/// Comma-separated repo-relative prefixes (forward slashes). Empty components
/// are ignored. Documented so hosts can widen the default without patching the
/// binary; the defaults themselves are [`DEFAULT_INDEX_EXCLUDE_PREFIXES`] and
/// the `testdata` directory-segment rule.
pub const INDEX_EXCLUDES_ENV: &str = "DEVMAP_INDEX_EXCLUDES";

/// Built-in path prefixes excluded from indexing (in addition to a `testdata`
/// directory segment anywhere in the path).
///
/// `vendor/grammars` holds tree-sitter C sources — including multi-megabyte
/// generated parsers — that are build inputs for this tool, not corpus for it.
pub const DEFAULT_INDEX_EXCLUDE_PREFIXES: &[&str] = &["vendor/grammars"];

/// Whether `rel_path` matches the default (or env-extended) index exclusions.
///
/// A `testdata` *directory* segment is always excluded — fixtures are not
/// coverage. A file *named* `testdata` at the leaves is not, matching the
/// fixture-path rule elsewhere.
pub fn is_default_index_excluded(rel_path: &str) -> bool {
    let norm = rel_path.replace('\\', "/").to_lowercase();
    let segments: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() >= 2 && segments[..segments.len() - 1].contains(&"testdata") {
        return true;
    }
    for prefix in DEFAULT_INDEX_EXCLUDE_PREFIXES {
        if norm == *prefix || norm.starts_with(&format!("{prefix}/")) {
            return true;
        }
    }
    if let Ok(extra) = std::env::var(INDEX_EXCLUDES_ENV) {
        for raw in extra.split(',') {
            let prefix = raw.trim().trim_matches('/').to_lowercase();
            if prefix.is_empty() {
                continue;
            }
            if norm == prefix || norm.starts_with(&format!("{prefix}/")) {
                return true;
            }
        }
    }
    false
}

/// The Swift *module* a file belongs to, derived from its path.
///
/// A Swift target is a module: every file under it shares one unqualified
/// namespace, and `import MarkDevKit` names that module, not a file. Same-module
/// files import each other not at all — the Java-package / Go-package shape.
///
/// Derived from the path, not from a build graph this kernel does not read:
///
/// * `Sources/<Name>/…` and `Tests/<Name>/…` (Swift Package Manager) → `Name`
/// * otherwise skip generic containers (`app`, `src`, `lib`, platform folders)
///   and take the next directory (`app/MarkDevKit/Editor/Foo.swift` → `MarkDevKit`)
///
/// The container list is a *path convention*, not a reserved module name.
/// SPM packages routinely call the target `App` or `Lib`; those sit in
/// `Sources/App` and `Sources/Lib`, and treating them as the same `app`/`lib`
/// folders the fallback skips left every file in those modules without a
/// module identity. Same-module lookup then never fired, and a bare `run()`
/// fell through to AmbiguousGlobal against every other `run` in the corpus.
///
/// `Package.swift` is a manifest, not a module member. A `.swift` file with no
/// remaining directory after those rules belongs to no module this function
/// can name, and same-module lookup simply does not fire for it.
pub fn swift_module_of(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if !path.ends_with(".swift") {
        return None;
    }
    let filename = path.rsplit('/').next().unwrap_or(&path);
    if filename == "Package.swift" {
        return None;
    }
    let mut dirs: Vec<&str> = path.split('/').collect();
    dirs.pop();
    for (index, segment) in dirs.iter().enumerate() {
        if matches!(*segment, "Sources" | "Tests") {
            if let Some(name) = dirs.get(index + 1).copied().filter(|name| !name.is_empty()) {
                return Some(name.to_string());
            }
        }
    }
    let remaining: Vec<&str> = dirs
        .into_iter()
        .filter(|segment| !is_swift_generic_container(segment))
        .collect();
    remaining
        .first()
        .copied()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn is_swift_generic_container(segment: &str) -> bool {
    matches!(
        segment.to_ascii_lowercase().as_str(),
        "app"
            | "src"
            | "lib"
            | "ios"
            | "macos"
            | "osx"
            | "watchos"
            | "tvos"
            | "ipados"
            | "visionos"
            | "catalyst"
            | "tests"
            | "sources"
    )
}

/// Whether a relative path should be indexed as source (not binary / build noise).
pub fn is_indexable_source(rel_path: &str) -> bool {
    let norm = rel_path.replace('\\', "/");
    if is_ignored_path(&norm) {
        return false;
    }

    let lang = detect_language(Path::new(&norm));
    !matches!(lang, "generic")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_authority_covers_all_specs_and_extractors() {
        // closes X8
        let required_alternate_extensions = [
            ".pyi", ".mts", ".cts", ".ets", ".phtml", ".mm", ".sc", ".dpr", ".cuh", ".cpy", ".hrl",
        ];
        for ext in &required_alternate_extensions {
            assert!(
                find_spec_by_extension(ext).is_some(),
                "Dropped alternate extension missing from LanguageSpec: {}",
                ext
            );
        }

        // Assert every spec routes to a named extractor
        for spec in LANGUAGE_SPECS {
            assert!(!spec.extractor_id.name().is_empty());
        }
    }

    /// Every extractor's name is pinned, distinct, and reachable from a spec.
    ///
    /// `ExtractorId::name` is the language identity written into the extraction
    /// cache key, the generation store, and every `language` filter in the query
    /// layer. Replacing the whole body with a single constant passed the old
    /// `!is_empty()` check, and that collapse would make every language share
    /// one cache key — one language's payload served for another's file.
    /// Four extractors deliberately differ from the tree-sitter grammar they
    /// reuse (arkts on the typescript grammar, vbnet on vb, metal on cpp,
    /// terraform on hcl), so the mapping is pinned here rather than derived.
    #[test]
    fn every_extractor_name_is_pinned_distinct_and_reachable() {
        let expected: &[(ExtractorId, &str)] = &[
            (ExtractorId::TypeScript, "typescript"),
            (ExtractorId::Tsx, "tsx"),
            (ExtractorId::JavaScript, "javascript"),
            (ExtractorId::ArkTs, "arkts"),
            (ExtractorId::Python, "python"),
            (ExtractorId::Go, "go"),
            (ExtractorId::Rust, "rust"),
            (ExtractorId::Java, "java"),
            (ExtractorId::CSharp, "csharp"),
            (ExtractorId::VbNet, "vbnet"),
            (ExtractorId::Php, "php"),
            (ExtractorId::Ruby, "ruby"),
            (ExtractorId::C, "c"),
            (ExtractorId::Cpp, "cpp"),
            (ExtractorId::ObjC, "objc"),
            (ExtractorId::Metal, "metal"),
            (ExtractorId::Cuda, "cuda"),
            (ExtractorId::Swift, "swift"),
            (ExtractorId::Kotlin, "kotlin"),
            (ExtractorId::Scala, "scala"),
            (ExtractorId::Dart, "dart"),
            (ExtractorId::Svelte, "svelte"),
            (ExtractorId::Vue, "vue"),
            (ExtractorId::Astro, "astro"),
            (ExtractorId::Liquid, "liquid"),
            (ExtractorId::Pascal, "pascal"),
            (ExtractorId::Lua, "lua"),
            (ExtractorId::Luau, "luau"),
            (ExtractorId::R, "r"),
            (ExtractorId::Cfml, "cfml"),
            (ExtractorId::Cobol, "cobol"),
            (ExtractorId::Erlang, "erlang"),
            (ExtractorId::Solidity, "solidity"),
            (ExtractorId::Terraform, "terraform"),
            (ExtractorId::Nix, "nix"),
            (ExtractorId::GenericTreeSitter, "generic"),
        ];

        let mut seen = std::collections::BTreeSet::new();
        for (id, name) in expected {
            assert_eq!(id.name(), *name, "{id:?} must keep its pinned name");
            assert!(seen.insert(*name), "two extractors share the name {name}");
        }

        // A new variant added without a name entry here is caught, because every
        // extractor a spec routes to must be pinned above.
        for spec in LANGUAGE_SPECS {
            assert!(
                expected.iter().any(|(id, _)| *id == spec.extractor_id),
                "{} routes to unpinned extractor {:?}",
                spec.name,
                spec.extractor_id
            );
        }
    }

    /// Every non-grammar file type detects as itself, and every config
    /// filename as `config`.
    ///
    /// Each of these was independently deletable. The fallback arms are what
    /// keep a `.sql` or `.yaml` file from being labelled `generic`, which the
    /// query layer's `--language` filters and the per-language coverage counts
    /// both read. The config filenames are checked before the extension, so
    /// dropping one silently reclassifies `tsconfig.json` as plain `json` and
    /// `Cargo.toml` as plain `toml`.
    #[test]
    fn detect_language_pins_config_names_and_fallback_extensions() {
        for filename in [
            "pyproject.toml",
            "Cargo.toml",
            "package.json",
            "go.mod",
            "jsconfig.json",
            "tsconfig.json",
        ] {
            assert_eq!(
                detect_language(Path::new(filename)),
                "config",
                "{filename} must detect as config, not by extension"
            );
            // Also when nested, since detection reads the file name not the path.
            assert_eq!(
                detect_language(&Path::new("pkg/sub").join(filename)),
                "config",
                "nested {filename} must detect as config"
            );
        }

        // A file that merely shares the extension is not a config file.
        assert_eq!(detect_language(Path::new("data.toml")), "toml");
        assert_eq!(detect_language(Path::new("data.json")), "json");

        for (ext, expected) in [
            ("sh", "shell"),
            ("bash", "shell"),
            ("zsh", "shell"),
            ("html", "html"),
            ("htm", "html"),
            ("css", "css"),
            ("scss", "css"),
            ("less", "css"),
            ("sql", "sql"),
            ("yaml", "yaml"),
            ("yml", "yaml"),
            ("json", "json"),
            ("toml", "toml"),
            ("md", "markdown"),
            ("markdown", "markdown"),
        ] {
            assert_eq!(
                detect_language(Path::new(&format!("doc.{ext}"))),
                expected,
                ".{ext} must detect as {expected}"
            );
        }

        // An unknown extension, and no extension at all, fall back to generic.
        assert_eq!(detect_language(Path::new("thing.qqq")), "generic");
        assert_eq!(detect_language(Path::new("LICENSE")), "generic");
    }

    /// Dotted directories are pruned, but the walk root itself is not.
    ///
    /// The `.`-prefix branch is the only thing that prunes tooling directories
    /// outside the hard-coded list (`.cache`, `.mypy_cache`, `.next`, …).
    /// Deleting its negation makes the branch unreachable and every such
    /// directory gets walked. Its `first != "."` clause is what keeps the walk
    /// root `.` admissible — without it `is_ignored_path(".")` is true and a
    /// walk rooted at `.` prunes itself, indexing nothing.
    ///
    /// (`&&` -> `||` at the outer guard is an equivalent mutant: the inner
    /// condition can only hold when the outer one does, since `first` is
    /// `norm`'s first path component.)
    #[test]
    fn dotted_directories_are_pruned_but_the_walk_root_is_not() {
        for dotted in [".cache/blob", ".mypy_cache/x/y", ".next/build/out"] {
            assert!(
                is_ignored_path(dotted),
                "{dotted} is a tooling directory and must be pruned"
            );
        }

        assert!(
            !is_ignored_path("."),
            "the walk root must stay admissible, or a walk rooted at `.` indexes nothing"
        );
        assert!(
            !is_ignored_path("./src/main.rs"),
            "a `./`-prefixed source is admissible"
        );
        assert!(
            !is_ignored_path("src/.keep"),
            "a dotfile below the root is not a dotted dir"
        );
    }

    #[test]
    fn detects_python_and_skips_target() {
        assert_eq!(detect_language(Path::new("pkg/mod.py")), "python");
        assert!(!is_indexable_source("target/debug/foo"));
        assert!(!is_indexable_source(".devcouncil/state.sqlite"));
        assert!(is_ignored_path(".devcouncil/codeintel/index.sqlite-wal"));
        assert!(is_ignored_path("pkg/node_modules/dependency.js"));
        assert!(!is_ignored_path("src/main.rs"));
        assert!(is_indexable_source("src/main.rs"));
        assert!(is_indexable_source("Cargo.toml"));
    }

    /// Discovery, not extraction, was dropping these.
    ///
    /// `is_indexable_source` admits a path only when `detect_language` names
    /// it, so an unnamed extension is invisible before any extractor runs —
    /// tier-2 recovery cannot rescue a file the walk never yields. Both of
    /// these are source with real declarations and no linked grammar, and a
    /// scan of one working tree found 19 `.proto` and 17 `.ps1` files that the
    /// graph could not see at all.
    #[test]
    fn grammarless_source_languages_are_still_discovered() {
        for (path, language) in [
            ("api/v1/user.proto", "protobuf"),
            ("scripts/deploy.ps1", "powershell"),
            ("scripts/mod.psm1", "powershell"),
            ("scripts/mod.psd1", "powershell"),
        ] {
            assert_eq!(detect_language(Path::new(path)), language, "{path}");
            assert!(
                is_indexable_source(path),
                "{path} must reach extraction to be recoverable by tier 2"
            );
        }
    }

    #[test]
    fn swift_module_of_follows_spm_then_the_target_directory() {
        assert_eq!(
            swift_module_of("Sources/App/main.swift").as_deref(),
            Some("App")
        );
        assert_eq!(
            swift_module_of("Sources/Lib/Core.swift").as_deref(),
            Some("Lib"),
            "SPM module names are not the app/src/lib path convention"
        );
        assert_eq!(
            swift_module_of("Tests/AppTests/AppTests.swift").as_deref(),
            Some("AppTests")
        );
        assert_eq!(
            swift_module_of("app/MarkDevKit/Editor/Foo.swift").as_deref(),
            Some("MarkDevKit")
        );
        assert_eq!(
            swift_module_of("desktop/Sources/Tauri/Host.swift").as_deref(),
            Some("Tauri")
        );
        assert_eq!(swift_module_of("Package.swift"), None);
        assert_eq!(swift_module_of("Main.swift"), None);
        assert_eq!(swift_module_of("src/lib.rs"), None);
    }
}
