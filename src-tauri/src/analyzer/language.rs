use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanguageInfo {
    pub name: &'static str,
    pub color_hex: &'static str,
    pub category: &'static str, // "programming", "markup", "data", "prose"
}

impl LanguageInfo {
    pub fn is_programming(self) -> bool {
        self.category == "programming"
    }
}

const RUST: LanguageInfo = LanguageInfo {
    name: "Rust",
    color_hex: "#dea584",
    category: "programming",
};
const TEXT: LanguageInfo = LanguageInfo {
    name: "Text",
    color_hex: "#808080",
    category: "prose",
};
const IMAGE: LanguageInfo = LanguageInfo {
    name: "Image",
    color_hex: "#a2d9ff",
    category: "data",
};
const BINARY: LanguageInfo = LanguageInfo {
    name: "Binary",
    color_hex: "#4b5563",
    category: "data",
};

const IGNORED_DIR_COMPONENTS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".git",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".yarn",
    "bower_components",
    "Pods",
    ".gradle",
    ".next",
    ".nuxt",
    ".output",
    "site-packages",
];

const LOCKFILE_NAMES: &[&str] = &[
    "cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "pnpm-lock.yml",
    "bun.lock",
    "bun.lockb",
    "npm-shrinkwrap.json",
    "go.sum",
    "composer.lock",
    "pipfile.lock",
    "poetry.lock",
    "gemfile.lock",
    "flake.lock",
];

/// Detects file language and assigns GitHub Linguist color palette.
pub struct LanguageDetector;

impl LanguageDetector {
    pub fn detect_from_bytes(file_path: &str, bytes: &[u8]) -> LanguageInfo {
        if looks_like_jpeg(bytes)
            || looks_like_png(bytes)
            || looks_like_gif(bytes)
            || looks_like_webp(bytes)
        {
            return IMAGE;
        }
        if bytes.len() >= 5 && bytes.starts_with(b"%PDF") {
            return LanguageInfo {
                name: "PDF",
                color_hex: "#b30b00",
                category: "data",
            };
        }
        if Self::looks_binary(bytes) {
            let from_path = Self::detect_from_path(file_path);
            if from_path.is_programming() {
                return from_path;
            }
            return BINARY;
        }
        let from_path = Self::detect_from_path(file_path);
        if from_path.name != TEXT.name {
            return from_path;
        }
        if let Some(from_shebang) = shebang_language(bytes) {
            return from_shebang;
        }
        from_path
    }

    pub fn is_image_path(file_path: &str) -> bool {
        Self::detect_from_path(file_path).name == IMAGE.name
    }

    pub fn detect_from_path(file_path: &str) -> LanguageInfo {
        let normalized = Self::normalize_rel_path(file_path);
        let file_name = file_name_of(&normalized);
        let file_lower = file_name.to_ascii_lowercase();

        if let Some(info) = filename_language(&file_lower) {
            return info;
        }
        if let Some(info) = compound_extension_language(&file_lower) {
            return info;
        }
        extension_language(extension_of(&file_lower))
    }

    pub fn normalize_rel_path(path: &str) -> String {
        let mut p = path.replace('\\', "/");
        while p.starts_with("./") {
            p = p[2..].to_string();
        }
        p.trim().trim_matches('/').to_string()
    }

    pub fn is_ignored_source_path(path: &str) -> bool {
        let p = Self::normalize_rel_path(path);
        if p.is_empty() {
            return true;
        }
        let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        if parts.len() < 2 {
            return false;
        }
        parts[..parts.len() - 1].iter().any(|c| {
            IGNORED_DIR_COMPONENTS
                .iter()
                .any(|skip| skip.eq_ignore_ascii_case(c))
        })
    }

    pub fn is_lockfile_path(path: &str) -> bool {
        let p = Self::normalize_rel_path(path);
        let name = file_name_of(&p).to_ascii_lowercase();
        LOCKFILE_NAMES.contains(&name.as_str())
    }

    pub fn is_generated_path(path: &str) -> bool {
        let p = Self::normalize_rel_path(path);
        let name = file_name_of(&p).to_ascii_lowercase();
        name.ends_with(".min.js")
            || name.ends_with(".min.mjs")
            || name.ends_with(".min.cjs")
            || name.ends_with(".min.css")
            || name.ends_with(".js.map")
            || name.ends_with(".css.map")
            || name.ends_with(".pb.go")
            || name.ends_with("_pb2.py")
            || name.ends_with("_pb2_grpc.py")
            || name.ends_with(".generated.ts")
            || name.ends_with(".generated.js")
            || name.ends_with(".generated.rs")
            || name.ends_with(".g.dart")
    }

    pub fn should_count_for_stats(path: &str, info: &LanguageInfo) -> bool {
        if Self::is_ignored_source_path(path)
            || Self::is_lockfile_path(path)
            || Self::is_generated_path(path)
        {
            return false;
        }
        !matches!(info.name, "Image" | "PDF" | "Binary")
    }

    pub fn comment_prefix(lang: &str) -> Option<&'static str> {
        match lang {
            "Python" | "YAML" | "TOML" | "Shell" | "Ruby" | "Perl" | "R" | "Makefile"
            | "Dockerfile" | "Elixir" | "Nim" | "Terraform" => Some("#"),
            "CSS" | "Less" | "Sass" => Some("/*"),
            "HTML" | "XML" | "Markdown" | "Vue" | "Svelte" => Some("<!--"),
            "Lua" | "SQL" | "Haskell" | "Elm" => Some("--"),
            "Lisp" | "Clojure" | "Scheme" | "Racket" => Some(";"),
            "Erlang" | "Matlab" => Some("%"),
            "Fortran" => Some("!"),
            _ => Some("//"),
        }
    }

    pub fn coverage_family(lang: &str) -> Option<&'static str> {
        match lang {
            "Rust" => Some("rust"),
            "JavaScript" | "TypeScript" | "TSX" | "JSX" | "Svelte" | "Vue" | "Astro" => {
                Some("javascript")
            }
            "Python" => Some("python"),
            "Go" => Some("go"),
            "Java" | "Kotlin" | "Scala" => Some("jvm"),
            "C" | "C++" | "Objective-C" | "Zig" => Some("native"),
            "Swift" => Some("swift"),
            _ => None,
        }
    }

    /// Seeds coverage / ecosystem scans from source *or* manifests.
    /// `Cargo.toml` is data/TOML for the language bar, but it still means "this tree is Rust".
    pub fn coverage_family_hint(path: &str, info: &LanguageInfo) -> Option<&'static str> {
        if Self::is_rust_source_or_manifest(path) {
            return Some("rust");
        }
        Self::coverage_family(info.name)
    }

    pub fn is_rust_source_or_manifest(path: &str) -> bool {
        let p = Self::normalize_rel_path(path);
        let name = file_name_of(&p).to_ascii_lowercase();
        matches!(
            name.as_str(),
            "cargo.toml" | "rust-toolchain" | "rust-toolchain.toml" | "clippy.toml"
        ) || name.ends_with(".rs")
            || name.ends_with(".rs.in")
    }

    pub fn ecosystem_hint(path: &str) -> Option<&'static str> {
        let p = Self::normalize_rel_path(path);
        let name = file_name_of(&p);
        match name {
            "package.json"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lock"
            | "bun.lockb"
            | "npm-shrinkwrap.json" => Some("npm"),
            "Cargo.toml" | "Cargo.lock" => Some("cargo"),
            "go.mod" | "go.sum" => Some("go"),
            "pyproject.toml" | "requirements.txt" | "Pipfile" | "Pipfile.lock" | "poetry.lock" => {
                Some("python")
            }
            "Gemfile" | "Gemfile.lock" => Some("ruby"),
            "composer.json" | "composer.lock" => Some("php"),
            _ => {
                if Self::is_rust_source_or_manifest(&p) {
                    Some("cargo")
                } else {
                    None
                }
            }
        }
    }

    pub fn looks_binary(bytes: &[u8]) -> bool {
        let probe = &bytes[..bytes.len().min(8192)];
        probe.contains(&0)
    }

    /// When more files exist than the read cap, keep every programming language
    /// represented (so Rust cannot vanish behind a wall of JSON/Markdown).
    pub fn prioritize_for_stats(
        candidates: Vec<(String, LanguageInfo)>,
        max_files: usize,
    ) -> Vec<(String, LanguageInfo)> {
        if candidates.len() <= max_files {
            return candidates;
        }
        let mut by_lang: BTreeMap<&'static str, Vec<(String, LanguageInfo)>> = BTreeMap::new();
        for item in candidates {
            by_lang.entry(item.1.name).or_default().push(item);
        }
        let mut programming = Vec::new();
        let mut other = Vec::new();
        for (name, items) in &by_lang {
            if items
                .first()
                .is_some_and(|item| item.1.category == "programming")
            {
                programming.push(*name);
            } else {
                other.push(*name);
            }
        }

        let mut out =
            Vec::with_capacity(max_files.min(by_lang.values().map(|v| v.len()).sum::<usize>()));
        let mut taken: BTreeMap<&'static str, usize> = BTreeMap::new();

        for name in programming.iter().chain(other.iter()) {
            if out.len() >= max_files {
                break;
            }
            if let Some(item) = by_lang.get(name).and_then(|v| v.first()) {
                out.push(item.clone());
                taken.insert(*name, 1);
            }
        }
        round_robin_rest(&mut out, &by_lang, &programming, &mut taken, max_files);
        round_robin_rest(&mut out, &by_lang, &other, &mut taken, max_files);
        out
    }
}

fn round_robin_rest(
    out: &mut Vec<(String, LanguageInfo)>,
    by_lang: &BTreeMap<&'static str, Vec<(String, LanguageInfo)>>,
    names: &[&'static str],
    taken: &mut BTreeMap<&'static str, usize>,
    max_files: usize,
) {
    loop {
        if out.len() >= max_files {
            return;
        }
        let mut progressed = false;
        for name in names {
            if out.len() >= max_files {
                return;
            }
            let idx = taken.entry(*name).or_insert(0);
            if let Some(item) = by_lang.get(*name).and_then(|v| v.get(*idx)) {
                out.push(item.clone());
                *idx += 1;
                progressed = true;
            }
        }
        if !progressed {
            return;
        }
    }
}

fn file_name_of(normalized: &str) -> &str {
    normalized.rsplit('/').next().unwrap_or(normalized)
}

fn extension_of(file_lower: &str) -> &str {
    if file_lower.starts_with('.') && !file_lower[1..].contains('.') {
        return "";
    }
    Path::new(file_lower)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
}

fn looks_like_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

fn looks_like_png(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
}

fn looks_like_gif(bytes: &[u8]) -> bool {
    bytes.len() >= 6 && bytes.starts_with(b"GIF8")
}

fn looks_like_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP"
}

fn filename_language(file_lower: &str) -> Option<LanguageInfo> {
    Some(match file_lower {
        "dockerfile" => LanguageInfo {
            name: "Dockerfile",
            color_hex: "#384d54",
            category: "programming",
        },
        "makefile" | "gnumakefile" => LanguageInfo {
            name: "Makefile",
            color_hex: "#427819",
            category: "programming",
        },
        "cmakelists.txt" => LanguageInfo {
            name: "CMake",
            color_hex: "#da3434",
            category: "programming",
        },
        "cargo.toml" | "clippy.toml" => LanguageInfo {
            name: "TOML",
            color_hex: "#9c4221",
            category: "data",
        },
        "rust-toolchain" | "rust-toolchain.toml" => LanguageInfo {
            name: "TOML",
            color_hex: "#9c4221",
            category: "data",
        },
        "go.mod" | "go.sum" => LanguageInfo {
            name: "Go",
            color_hex: "#00add8",
            category: "data",
        },
        "gemfile" => LanguageInfo {
            name: "Ruby",
            color_hex: "#701516",
            category: "programming",
        },
        ".gitignore" | ".gitattributes" | ".gitmodules" => LanguageInfo {
            name: "Git Config",
            color_hex: "#f14e32",
            category: "data",
        },
        _ => return None,
    })
}

fn compound_extension_language(file_lower: &str) -> Option<LanguageInfo> {
    if file_lower.ends_with(".rs.in") {
        return Some(RUST);
    }
    if file_lower.ends_with(".d.ts")
        || file_lower.ends_with(".d.mts")
        || file_lower.ends_with(".d.cts")
    {
        return Some(LanguageInfo {
            name: "TypeScript",
            color_hex: "#3178c6",
            category: "programming",
        });
    }
    None
}

fn extension_language(ext: &str) -> LanguageInfo {
    match ext {
        "rs" => RUST,
        "ts" | "mts" | "cts" => LanguageInfo {
            name: "TypeScript",
            color_hex: "#3178c6",
            category: "programming",
        },
        "tsx" => LanguageInfo {
            name: "TSX",
            color_hex: "#3178c6",
            category: "programming",
        },
        "js" | "mjs" | "cjs" => LanguageInfo {
            name: "JavaScript",
            color_hex: "#f1e05a",
            category: "programming",
        },
        "jsx" => LanguageInfo {
            name: "JSX",
            color_hex: "#f1e05a",
            category: "programming",
        },
        "svelte" => LanguageInfo {
            name: "Svelte",
            color_hex: "#ff3e00",
            category: "programming",
        },
        "vue" => LanguageInfo {
            name: "Vue",
            color_hex: "#41b883",
            category: "programming",
        },
        "astro" => LanguageInfo {
            name: "Astro",
            color_hex: "#ff5a03",
            category: "programming",
        },
        "go" => LanguageInfo {
            name: "Go",
            color_hex: "#00add8",
            category: "programming",
        },
        "py" | "pyi" | "pyw" => LanguageInfo {
            name: "Python",
            color_hex: "#3572a5",
            category: "programming",
        },
        "c" | "h" => LanguageInfo {
            name: "C",
            color_hex: "#555555",
            category: "programming",
        },
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => LanguageInfo {
            name: "C++",
            color_hex: "#f34b7d",
            category: "programming",
        },
        "m" => LanguageInfo {
            name: "Objective-C",
            color_hex: "#438eff",
            category: "programming",
        },
        "mm" => LanguageInfo {
            name: "Objective-C++",
            color_hex: "#6866fb",
            category: "programming",
        },
        "java" => LanguageInfo {
            name: "Java",
            color_hex: "#b07219",
            category: "programming",
        },
        "kt" | "kts" => LanguageInfo {
            name: "Kotlin",
            color_hex: "#a97bff",
            category: "programming",
        },
        "swift" => LanguageInfo {
            name: "Swift",
            color_hex: "#f05138",
            category: "programming",
        },
        "scala" | "sc" => LanguageInfo {
            name: "Scala",
            color_hex: "#c22d40",
            category: "programming",
        },
        "rb" => LanguageInfo {
            name: "Ruby",
            color_hex: "#701516",
            category: "programming",
        },
        "php" => LanguageInfo {
            name: "PHP",
            color_hex: "#4f5d95",
            category: "programming",
        },
        "cs" => LanguageInfo {
            name: "C#",
            color_hex: "#178600",
            category: "programming",
        },
        "fs" | "fsi" | "fsx" => LanguageInfo {
            name: "F#",
            color_hex: "#b845fc",
            category: "programming",
        },
        "ex" | "exs" => LanguageInfo {
            name: "Elixir",
            color_hex: "#6e4a7e",
            category: "programming",
        },
        "erl" | "hrl" => LanguageInfo {
            name: "Erlang",
            color_hex: "#b83998",
            category: "programming",
        },
        "hs" => LanguageInfo {
            name: "Haskell",
            color_hex: "#5e5086",
            category: "programming",
        },
        "lua" => LanguageInfo {
            name: "Lua",
            color_hex: "#000080",
            category: "programming",
        },
        "r" => LanguageInfo {
            name: "R",
            color_hex: "#198ce7",
            category: "programming",
        },
        "dart" => LanguageInfo {
            name: "Dart",
            color_hex: "#00b4ab",
            category: "programming",
        },
        "zig" => LanguageInfo {
            name: "Zig",
            color_hex: "#ec915c",
            category: "programming",
        },
        "nim" => LanguageInfo {
            name: "Nim",
            color_hex: "#ffc200",
            category: "programming",
        },
        "clj" | "cljs" | "cljc" => LanguageInfo {
            name: "Clojure",
            color_hex: "#db5855",
            category: "programming",
        },
        "ml" | "mli" => LanguageInfo {
            name: "OCaml",
            color_hex: "#ef7a08",
            category: "programming",
        },
        "elm" => LanguageInfo {
            name: "Elm",
            color_hex: "#60b5cc",
            category: "programming",
        },
        "sol" => LanguageInfo {
            name: "Solidity",
            color_hex: "#aa6746",
            category: "programming",
        },
        "html" | "htm" => LanguageInfo {
            name: "HTML",
            color_hex: "#e34c26",
            category: "markup",
        },
        "css" => LanguageInfo {
            name: "CSS",
            color_hex: "#563d7c",
            category: "markup",
        },
        "scss" | "sass" => LanguageInfo {
            name: "Sass",
            color_hex: "#a53b70",
            category: "markup",
        },
        "less" => LanguageInfo {
            name: "Less",
            color_hex: "#1d365d",
            category: "markup",
        },
        "xml" => LanguageInfo {
            name: "XML",
            color_hex: "#0060ac",
            category: "data",
        },
        "json" | "jsonc" | "json5" => LanguageInfo {
            name: "JSON",
            color_hex: "#292929",
            category: "data",
        },
        "toml" => LanguageInfo {
            name: "TOML",
            color_hex: "#9c4221",
            category: "data",
        },
        "yaml" | "yml" => LanguageInfo {
            name: "YAML",
            color_hex: "#cb171e",
            category: "data",
        },
        "md" | "markdown" | "mdx" => LanguageInfo {
            name: "Markdown",
            color_hex: "#083fa1",
            category: "prose",
        },
        "sql" => LanguageInfo {
            name: "SQL",
            color_hex: "#e38c00",
            category: "data",
        },
        "sh" | "bash" | "zsh" | "ksh" | "fish" => LanguageInfo {
            name: "Shell",
            color_hex: "#89e051",
            category: "programming",
        },
        "ps1" | "psm1" => LanguageInfo {
            name: "PowerShell",
            color_hex: "#012456",
            category: "programming",
        },
        "bat" | "cmd" => LanguageInfo {
            name: "Batchfile",
            color_hex: "#c1d18a",
            category: "programming",
        },
        "tf" | "tfvars" => LanguageInfo {
            name: "Terraform",
            color_hex: "#7b42bb",
            category: "programming",
        },
        "proto" => LanguageInfo {
            name: "Protocol Buffer",
            color_hex: "#e8353c",
            category: "data",
        },
        "graphql" | "gql" => LanguageInfo {
            name: "GraphQL",
            color_hex: "#e10098",
            category: "data",
        },
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" => IMAGE,
        _ => TEXT,
    }
}

fn shebang_language(bytes: &[u8]) -> Option<LanguageInfo> {
    if !bytes.starts_with(b"#!") {
        return None;
    }
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len())
        .min(256);
    let line = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
    let tokens: Vec<&str> = line
        .trim_start_matches("#!")
        .split(|c: char| c.is_whitespace() || c == '/')
        .filter(|t| {
            !t.is_empty() && *t != "usr" && *t != "bin" && *t != "env" && *t != "-s" && *t != "-c"
        })
        .collect();
    for token in tokens {
        if let Some(info) = interpreter_language(token) {
            return Some(info);
        }
    }
    None
}

fn interpreter_language(token: &str) -> Option<LanguageInfo> {
    if token.contains("rust-script") || token == "rustc" || token == "cargo" {
        return Some(RUST);
    }
    let interp = token.split('.').next().unwrap_or(token);
    Some(match interp {
        "python" | "python2" | "python3" | "pypy" | "pypy3" => LanguageInfo {
            name: "Python",
            color_hex: "#3572a5",
            category: "programming",
        },
        "node" | "nodejs" | "deno" | "bun" => LanguageInfo {
            name: "JavaScript",
            color_hex: "#f1e05a",
            category: "programming",
        },
        "bash" | "sh" | "zsh" | "ksh" | "dash" | "fish" => LanguageInfo {
            name: "Shell",
            color_hex: "#89e051",
            category: "programming",
        },
        "ruby" | "jruby" => LanguageInfo {
            name: "Ruby",
            color_hex: "#701516",
            category: "programming",
        },
        "perl" => LanguageInfo {
            name: "Perl",
            color_hex: "#0298c3",
            category: "programming",
        },
        "lua" => LanguageInfo {
            name: "Lua",
            color_hex: "#000080",
            category: "programming",
        },
        "php" => LanguageInfo {
            name: "PHP",
            color_hex: "#4f5d95",
            category: "programming",
        },
        "ts-node" => LanguageInfo {
            name: "TypeScript",
            color_hex: "#3178c6",
            category: "programming",
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_detection() {
        assert_eq!(
            LanguageDetector::detect_from_path("src/main.rs").name,
            "Rust"
        );
        assert_eq!(
            LanguageDetector::detect_from_path("src/App.svelte").name,
            "Svelte"
        );
        assert_eq!(
            LanguageDetector::detect_from_path("package.json").name,
            "JSON"
        );
        assert_eq!(
            LanguageDetector::detect_from_path("docs/README.md").name,
            "Markdown"
        );
        assert_eq!(
            LanguageDetector::detect_from_bytes(
                "logo.bin",
                &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
            )
            .name,
            "Image"
        );
    }

    #[test]
    fn rust_is_detected_in_nested_tauri_and_odd_paths() {
        assert_eq!(
            LanguageDetector::detect_from_path("src-tauri/src/lib.rs").name,
            "Rust"
        );
        assert_eq!(
            LanguageDetector::detect_from_path(r"src-tauri\src\main.rs").name,
            "Rust"
        );
        assert_eq!(
            LanguageDetector::detect_from_path("SRC/MAIN.RS").name,
            "Rust"
        );
        assert_eq!(
            LanguageDetector::detect_from_path("./crates/foo/src/lib.rs").name,
            "Rust"
        );
        assert_eq!(
            LanguageDetector::detect_from_path("src/foo.rs.in").name,
            "Rust"
        );
        assert_eq!(LanguageDetector::detect_from_path("build.rs").name, "Rust");
        assert_eq!(LanguageDetector::detect_from_path("mod.rs").name, "Rust");
    }

    #[test]
    fn cargo_manifests_are_toml_but_seed_rust_family() {
        let cargo = LanguageDetector::detect_from_path("src-tauri/Cargo.toml");
        assert_eq!(cargo.name, "TOML");
        assert_eq!(cargo.category, "data");
        assert_eq!(
            LanguageDetector::coverage_family_hint("src-tauri/Cargo.toml", &cargo),
            Some("rust")
        );
        assert_eq!(
            LanguageDetector::ecosystem_hint("src-tauri/Cargo.toml"),
            Some("cargo")
        );
        assert!(LanguageDetector::is_rust_source_or_manifest("Cargo.toml"));
        assert!(!LanguageDetector::should_count_for_stats(
            "Cargo.lock",
            &LanguageDetector::detect_from_path("Cargo.lock")
        ));
        assert!(LanguageDetector::is_lockfile_path("src-tauri/Cargo.lock"));
        assert!(LanguageDetector::is_lockfile_path("package-lock.json"));
    }

    #[test]
    fn rust_script_shebang_without_extension() {
        let info = LanguageDetector::detect_from_bytes(
            "run",
            b"#!/usr/bin/env rust-script\nfn main() {}\n",
        );
        assert_eq!(info.name, "Rust");
        let cargo = LanguageDetector::detect_from_bytes(
            "tool",
            b"#!/usr/bin/env -S cargo +nightly -Zscript\nfn main() {}\n",
        );
        assert_eq!(cargo.name, "Rust");
    }

    #[test]
    fn ignored_dirs_skip_build_trees_not_src_tauri_or_crate_names() {
        assert!(LanguageDetector::is_ignored_source_path(
            "target/debug/lib.rs"
        ));
        assert!(LanguageDetector::is_ignored_source_path(
            "src-tauri/target/foo.rs"
        ));
        assert!(LanguageDetector::is_ignored_source_path(
            "node_modules/foo/index.js"
        ));
        assert!(!LanguageDetector::is_ignored_source_path(
            "src-tauri/src/lib.rs"
        ));
        assert!(!LanguageDetector::is_ignored_source_path(
            "crates/target-spec/src/lib.rs"
        ));
        assert!(!LanguageDetector::is_ignored_source_path("src/target.rs"));
        assert!(!LanguageDetector::is_ignored_source_path("lib.rs"));
    }

    #[test]
    fn prioritize_keeps_rust_when_json_and_markdown_dominate() {
        let mut candidates = Vec::new();
        for i in 0..200 {
            candidates.push((
                format!("docs/{i:04}.md"),
                LanguageDetector::detect_from_path(&format!("docs/{i:04}.md")),
            ));
            candidates.push((
                format!("data/{i:04}.json"),
                LanguageDetector::detect_from_path(&format!("data/{i:04}.json")),
            ));
        }
        candidates.push((
            "src-tauri/src/lib.rs".into(),
            LanguageDetector::detect_from_path("src-tauri/src/lib.rs"),
        ));
        let selected = LanguageDetector::prioritize_for_stats(candidates, 50);
        assert!(
            selected
                .iter()
                .any(|(p, info)| p.ends_with(".rs") && info.name == "Rust"),
            "rust must survive a tiny read cap: {:?}",
            selected
                .iter()
                .map(|(p, i)| (p.as_str(), i.name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn coverage_family_maps_rust_and_ignores_prose() {
        assert_eq!(LanguageDetector::coverage_family("Rust"), Some("rust"));
        assert_eq!(LanguageDetector::coverage_family("Markdown"), None);
        let rs = LanguageDetector::detect_from_path("src/main.rs");
        assert_eq!(
            LanguageDetector::coverage_family_hint("src/main.rs", &rs),
            Some("rust")
        );
    }

    #[test]
    fn generated_and_minified_are_not_counted() {
        let js = LanguageDetector::detect_from_path("app.min.js");
        assert!(!LanguageDetector::should_count_for_stats(
            "dist/app.min.js",
            &js
        ));
        assert!(LanguageDetector::is_generated_path("app.min.js"));
        assert!(LanguageDetector::is_ignored_source_path("dist/app.js"));
    }

    #[test]
    fn comment_prefix_for_rust_is_line_comment() {
        assert_eq!(LanguageDetector::comment_prefix("Rust"), Some("//"));
        assert_eq!(LanguageDetector::comment_prefix("Python"), Some("#"));
    }

    #[test]
    fn ecosystem_hint_from_rust_source_file() {
        assert_eq!(
            LanguageDetector::ecosystem_hint("src/main.rs"),
            Some("cargo")
        );
        assert_eq!(
            LanguageDetector::ecosystem_hint("src-tauri/src/engine/git_reader.rs"),
            Some("cargo")
        );
    }
}
