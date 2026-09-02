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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanguageSpec {
    pub name: &'static str,
    pub grammar: &'static str,
    pub extensions: &'static [&'static str],
    pub embedded: &'static [&'static str],
    pub extractor_id: ExtractorId,
    pub lsp_id: &'static str,
    pub viz_color: &'static str,
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
    },
    LanguageSpec {
        name: "TSX",
        grammar: "tsx",
        extensions: &[".tsx"],
        embedded: &[],
        extractor_id: ExtractorId::Tsx,
        lsp_id: "typescript",
        viz_color: "#3178c6",
    },
    LanguageSpec {
        name: "JavaScript",
        grammar: "javascript",
        extensions: &[".js", ".jsx", ".mjs", ".cjs"],
        embedded: &[],
        extractor_id: ExtractorId::JavaScript,
        lsp_id: "javascript",
        viz_color: "#f7df1e",
    },
    LanguageSpec {
        name: "ArkTS",
        grammar: "typescript",
        extensions: &[".ets"],
        embedded: &[],
        extractor_id: ExtractorId::ArkTs,
        lsp_id: "typescript",
        viz_color: "#002b36",
    },
    LanguageSpec {
        name: "Python",
        grammar: "python",
        extensions: &[".py", ".pyi"],
        embedded: &[],
        extractor_id: ExtractorId::Python,
        lsp_id: "python",
        viz_color: "#3572A5",
    },
    LanguageSpec {
        name: "Go",
        grammar: "go",
        extensions: &[".go"],
        embedded: &[],
        extractor_id: ExtractorId::Go,
        lsp_id: "gopls",
        viz_color: "#00ADD8",
    },
    LanguageSpec {
        name: "Rust",
        grammar: "rust",
        extensions: &[".rs"],
        embedded: &[],
        extractor_id: ExtractorId::Rust,
        lsp_id: "rust-analyzer",
        viz_color: "#dea584",
    },
    LanguageSpec {
        name: "Java",
        grammar: "java",
        extensions: &[".java"],
        embedded: &[],
        extractor_id: ExtractorId::Java,
        lsp_id: "jdtls",
        viz_color: "#b07219",
    },
    LanguageSpec {
        name: "C#",
        grammar: "csharp",
        extensions: &[".cs"],
        embedded: &[],
        extractor_id: ExtractorId::CSharp,
        lsp_id: "omnisharp",
        viz_color: "#178600",
    },
    LanguageSpec {
        name: "VB.NET",
        grammar: "vb",
        extensions: &[".vb"],
        embedded: &[],
        extractor_id: ExtractorId::VbNet,
        lsp_id: "vbnet",
        viz_color: "#945db7",
    },
    LanguageSpec {
        name: "PHP",
        grammar: "php",
        extensions: &[".php", ".phtml"],
        embedded: &[],
        extractor_id: ExtractorId::Php,
        lsp_id: "intelephense",
        viz_color: "#4F5D95",
    },
    LanguageSpec {
        name: "Ruby",
        grammar: "ruby",
        extensions: &[".rb", ".rake"],
        embedded: &[],
        extractor_id: ExtractorId::Ruby,
        lsp_id: "solargraph",
        viz_color: "#701516",
    },
    LanguageSpec {
        name: "C",
        grammar: "c",
        extensions: &[".c", ".h"],
        embedded: &[],
        extractor_id: ExtractorId::C,
        lsp_id: "clangd",
        viz_color: "#555555",
    },
    LanguageSpec {
        name: "C++",
        grammar: "cpp",
        extensions: &[".cc", ".cpp", ".cxx", ".hh", ".hpp", ".hxx"],
        embedded: &[],
        extractor_id: ExtractorId::Cpp,
        lsp_id: "clangd",
        viz_color: "#f34b7d",
    },
    LanguageSpec {
        name: "Objective-C",
        grammar: "objc",
        extensions: &[".m", ".mm"],
        embedded: &[],
        extractor_id: ExtractorId::ObjC,
        lsp_id: "clangd",
        viz_color: "#438eff",
    },
    LanguageSpec {
        name: "Metal",
        grammar: "cpp",
        extensions: &[".metal"],
        embedded: &[],
        extractor_id: ExtractorId::Metal,
        lsp_id: "clangd",
        viz_color: "#8f14e9",
    },
    LanguageSpec {
        name: "CUDA",
        grammar: "cuda",
        extensions: &[".cu", ".cuh"],
        embedded: &[],
        extractor_id: ExtractorId::Cuda,
        lsp_id: "clangd",
        viz_color: "#3A4E3A",
    },
    LanguageSpec {
        name: "Swift",
        grammar: "swift",
        extensions: &[".swift"],
        embedded: &[],
        extractor_id: ExtractorId::Swift,
        lsp_id: "sourcekit-lsp",
        viz_color: "#F05138",
    },
    LanguageSpec {
        name: "Kotlin",
        grammar: "kotlin",
        extensions: &[".kt", ".kts"],
        embedded: &[],
        extractor_id: ExtractorId::Kotlin,
        lsp_id: "kotlin-language-server",
        viz_color: "#A97BFF",
    },
    LanguageSpec {
        name: "Scala",
        grammar: "scala",
        extensions: &[".scala", ".sc"],
        embedded: &[],
        extractor_id: ExtractorId::Scala,
        lsp_id: "metals",
        viz_color: "#c22d40",
    },
    LanguageSpec {
        name: "Dart",
        grammar: "dart",
        extensions: &[".dart"],
        embedded: &[],
        extractor_id: ExtractorId::Dart,
        lsp_id: "dart-analysis-server",
        viz_color: "#00B4AB",
    },
    LanguageSpec {
        name: "Svelte",
        grammar: "svelte",
        extensions: &[".svelte"],
        embedded: &["typescript", "javascript", "css", "html"],
        extractor_id: ExtractorId::Svelte,
        lsp_id: "svelte-language-server",
        viz_color: "#ff3e00",
    },
    LanguageSpec {
        name: "Vue",
        grammar: "vue",
        extensions: &[".vue"],
        embedded: &["typescript", "javascript", "css", "html"],
        extractor_id: ExtractorId::Vue,
        lsp_id: "volar",
        viz_color: "#41b883",
    },
    LanguageSpec {
        name: "Astro",
        grammar: "astro",
        extensions: &[".astro"],
        embedded: &["typescript", "javascript", "css", "html"],
        extractor_id: ExtractorId::Astro,
        lsp_id: "astro-ls",
        viz_color: "#ff5a03",
    },
    LanguageSpec {
        name: "Liquid",
        grammar: "liquid",
        extensions: &[".liquid"],
        embedded: &["html", "javascript", "css"],
        extractor_id: ExtractorId::Liquid,
        lsp_id: "theme-check",
        viz_color: "#67b8de",
    },
    LanguageSpec {
        name: "Pascal/Delphi",
        grammar: "pascal",
        extensions: &[".pas", ".pp", ".dpr"],
        embedded: &[],
        extractor_id: ExtractorId::Pascal,
        lsp_id: "pascal-lsp",
        viz_color: "#E3F171",
    },
    LanguageSpec {
        name: "Lua",
        grammar: "lua",
        extensions: &[".lua"],
        embedded: &[],
        extractor_id: ExtractorId::Lua,
        lsp_id: "lua-language-server",
        viz_color: "#000080",
    },
    LanguageSpec {
        name: "Luau",
        grammar: "luau",
        extensions: &[".luau"],
        embedded: &[],
        extractor_id: ExtractorId::Luau,
        lsp_id: "luau-lsp",
        viz_color: "#00A2FF",
    },
    LanguageSpec {
        name: "R",
        grammar: "r",
        extensions: &[".r", ".R"],
        embedded: &[],
        extractor_id: ExtractorId::R,
        lsp_id: "r-languageserver",
        viz_color: "#198CE7",
    },
    LanguageSpec {
        name: "CFML",
        grammar: "cfml",
        extensions: &[".cfm", ".cfc"],
        embedded: &[],
        extractor_id: ExtractorId::Cfml,
        lsp_id: "cfls",
        viz_color: "#224f80",
    },
    LanguageSpec {
        name: "COBOL",
        grammar: "cobol",
        extensions: &[".cob", ".cbl", ".cpy"],
        embedded: &[],
        extractor_id: ExtractorId::Cobol,
        lsp_id: "cobol-ls",
        viz_color: "#005ca5",
    },
    LanguageSpec {
        name: "Erlang",
        grammar: "erlang",
        extensions: &[".erl", ".hrl"],
        embedded: &[],
        extractor_id: ExtractorId::Erlang,
        lsp_id: "erlang-ls",
        viz_color: "#B83998",
    },
    LanguageSpec {
        name: "Solidity",
        grammar: "solidity",
        extensions: &[".sol"],
        embedded: &[],
        extractor_id: ExtractorId::Solidity,
        lsp_id: "solc",
        viz_color: "#AA6746",
    },
    LanguageSpec {
        name: "Terraform/OpenTofu",
        grammar: "hcl",
        extensions: &[".tf", ".tfvars", ".hcl"],
        embedded: &[],
        extractor_id: ExtractorId::Terraform,
        lsp_id: "terraform-ls",
        viz_color: "#5C4EE5",
    },
    LanguageSpec {
        name: "Nix",
        grammar: "nix",
        extensions: &[".nix"],
        embedded: &[],
        extractor_id: ExtractorId::Nix,
        lsp_id: "nil",
        viz_color: "#7e71de",
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

/// Whether a relative path is inside a VCS, build, environment, or devmap-owned
/// namespace. This is the shared admission boundary for cold walks and live
/// watcher events.
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
        if matches!(
            part,
            "target"
                | "node_modules"
                | ".git"
                | ".devcouncil"
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
    false
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
}
