//! The coverage toolchain vocabulary, owned in one place.
//!
//! Two subsystems have to agree, exactly, about what coverage generation is
//! allowed to do:
//!
//! * [`crate::analyzer::coverage`] — the *planner*. It decides which commands
//!   to publish to the UI as `setup_commands` / `suggested_commands`.
//! * [`crate::terminal`] — the *gate*. It decides which argv may actually be
//!   spawned under [`crate::terminal::ManviActionKind::CoverageGenerator`].
//!
//! They used to carry private copies of the same facts. The pytest package
//! set existed twice (`&["pytest", "pytest-cov"]` in the gate, the string
//! `"pytest pytest-cov"` in the planner); the vitest provider names existed
//! twice; the four virtualenv interpreter spellings existed twice; the
//! repository-local program list existed in the gate while the planner
//! open-coded the same paths.
//!
//! Copies of a fact do not fail loudly when they disagree — they fail as a
//! button that does nothing. A command the planner publishes but the gate
//! refuses reaches the user as a *plan* that is then rejected the instant it
//! is pressed. That has already happened twice in this codebase: the vendored
//! `vendor/bin/phpunit` step was refused with "executable paths are not
//! allowed", and `bundle install` was refused as "outside the purpose-specific
//! command allowlist" — so PHP and Ruby coverage could never have run, and
//! nothing detected it because each side was internally consistent.
//!
//! So there is one leader. The planner *builds* its command text from these
//! definitions and the gate *validates* against the same ones, which makes a
//! drift a compile error rather than a dead button. What cannot be expressed
//! as a shared constant is covered by the contract test in
//! [`crate::analyzer::coverage`], which runs the real planner over fixture
//! repositories and asserts the gate accepts every command it published.

/// The Python packages a coverage generate step may install, and nothing else.
///
/// This is the entire blast radius of the `pip install` exception: the gate
/// refuses any package outside this set, and the planner cannot name one,
/// because it builds the command text from the very same slice.
pub(crate) const PYTEST_PACKAGES: &[&str] = &["pytest", "pytest-cov"];

/// The tail of `<venv python> -m pip install …`, rendered from
/// [`PYTEST_PACKAGES`] rather than written out a second time.
pub(crate) fn pytest_install_arguments() -> String {
    PYTEST_PACKAGES.join(" ")
}

/// The Python package that turns gcov data into lcov, and nothing else.
///
/// Same rule as [`PYTEST_PACKAGES`]: this slice is the entire blast radius of
/// the C/C++ `pip install` step, the gate refuses anything outside it, and the
/// planner renders its command text from the same values.
pub(crate) const GCOVR_PACKAGES: &[&str] = &["gcovr"];

/// The tail of `<venv python> -m pip install …` for the C/C++ toolchain.
pub(crate) fn gcovr_install_arguments() -> String {
    GCOVR_PACKAGES.join(" ")
}

/// The module name `<venv python> -m …` invokes to run gcovr.
///
/// gcovr ships `gcovr/__main__.py`, so the module form works wherever the
/// package is importable. It is preferred over the `gcovr` console script
/// because the gate already understands `<venv python> -m <module>` and does
/// not have to learn a new repository-local program spelling.
pub(crate) const GCOVR_MODULE: &str = "gcovr";

/// Out-of-tree build directory GitPulse configures for an instrumented C/C++
/// build.
///
/// Deliberately NOT the project's own `build/`. GitPulse compiles with
/// coverage instrumentation and a forced `CMAKE_BUILD_TYPE`; doing that in the
/// directory a developer builds from would silently replace their build with a
/// slower instrumented one. A separate directory also means the whole
/// experiment is removable with one `rm -rf`, and it keeps this app out of the
/// project's own CMake files — which is the reason C/C++ coverage was declared
/// unplannable before.
pub(crate) const NATIVE_COVERAGE_BUILD_DIR: &str = "build-gitpulse-coverage";

/// Where gcovr writes lcov inside [`NATIVE_COVERAGE_BUILD_DIR`].
///
/// Inside the build directory on purpose: `cmake -B` creates that directory,
/// so nothing has to `mkdir` first, and the path cannot collide with the
/// root-level `lcov.info` that the Rust family also claims.
pub(crate) const NATIVE_COVERAGE_LCOV: &str = "build-gitpulse-coverage/lcov.info";

/// The one compiler and linker flag that enables gcov instrumentation.
///
/// `--coverage` is the documented shorthand GCC and Clang both accept; it
/// implies `-fprofile-arcs -ftest-coverage` when compiling and links `libgcov`.
/// Passing it through `-D CMAKE_*_FLAGS` on the configure command line is what
/// makes this work without editing the project's `CMakeLists.txt`.
pub(crate) const NATIVE_COVERAGE_FLAG: &str = "--coverage";

/// JavaScript coverage providers GitPulse knows how to drive. Appearing here
/// is what makes a provider both installable by the gate and recognizable by
/// the planner when it decides whether one is already declared.
pub(crate) const JS_COVERAGE_PROVIDERS: &[&str] =
    &["@vitest/coverage-v8", "@vitest/coverage-istanbul"];

/// The provider the planner installs when a project has vitest but no
/// provider. Necessarily one of [`JS_COVERAGE_PROVIDERS`] — it is that slice's
/// first element rather than a repeat of the name.
pub(crate) const DEFAULT_JS_COVERAGE_PROVIDER: &str = JS_COVERAGE_PROVIDERS[0];

/// Directory names a project virtualenv may occupy.
///
/// Pinned to the two conventional spellings rather than "any repository
/// path": coverage generation has exactly one reason to create a virtualenv,
/// and a free-form target would let plan text scatter interpreters through
/// the checkout.
pub(crate) const VENV_DIR_NAMES: &[&str] = &[".venv", "venv"];

/// Where GitPulse creates a virtualenv when a project has none.
pub(crate) const MANAGED_VENV_DIR: &str = VENV_DIR_NAMES[0];

/// Every spelling of a virtualenv interpreter the planner may discover and
/// the gate may execute, in probe order.
///
/// The Windows entries are cased and suffixed the way the interpreter is
/// really named on disk; the gate compares them after
/// [`crate::terminal::normalized_program`] has folded case and stripped the
/// executable suffix.
pub(crate) const VENV_PYTHON_RELPATHS: &[&str] = &[
    ".venv/bin/python",
    "venv/bin/python",
    ".venv/Scripts/python.exe",
    "venv/Scripts/python.exe",
];

/// The interpreter inside [`MANAGED_VENV_DIR`]. Kept beside the directory
/// constant so the create step and every later step cannot drift apart.
pub(crate) fn managed_venv_python() -> &'static str {
    if cfg!(windows) {
        ".venv/Scripts/python.exe"
    } else {
        ".venv/bin/python"
    }
}

/// Executables that live inside the open repository and may be named as the
/// program, mapped to the tool identity the allowlist reasons about.
///
/// Exact-string only — never a prefix, glob, or "anything under vendor/". A
/// repository must not be able to nominate an arbitrary checked-in file as an
/// executable by naming it, and the gate separately proves the file really
/// resolves inside the repository before it is spawned.
///
/// Takes an already-normalized program (lowercased, executable suffix
/// stripped), which is why the Windows virtualenv spellings appear here in
/// lowercase without `.exe`.
pub(crate) fn repo_local_program(normalized: &str) -> Option<&'static str> {
    match normalized {
        // Build wrappers a project checks in so nobody needs a system tool.
        "./gradlew" | ".\\gradlew" => Some("./gradlew"),
        "./mvnw" | ".\\mvnw" => Some("./mvnw"),
        ".venv/bin/python" | "venv/bin/python" | ".venv/scripts/python" | "venv/scripts/python" => {
            Some("python")
        }
        "vendor/bin/phpunit" => Some("phpunit"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::normalized_program;

    /// The planner reaches for [`managed_venv_python`] when it creates a
    /// virtualenv and for [`VENV_PYTHON_RELPATHS`] when it discovers one. If
    /// the created interpreter is not a discoverable one, GitPulse builds a
    /// virtualenv it will not find on the next scan.
    #[test]
    fn the_managed_interpreter_is_one_the_planner_can_rediscover() {
        assert!(
            VENV_PYTHON_RELPATHS.contains(&managed_venv_python()),
            "managed_venv_python() must be probed by existing_venv_python()"
        );
        assert!(
            managed_venv_python().starts_with(&format!("{MANAGED_VENV_DIR}/")),
            "the managed interpreter must live in the managed venv directory"
        );
        assert!(VENV_DIR_NAMES.contains(&MANAGED_VENV_DIR));
    }

    /// Every interpreter the planner may name must be one the gate will
    /// accept as a repository-local program. This is the assertion that
    /// would have caught a Windows spelling drifting away from the gate's
    /// lowercase match arm.
    #[test]
    fn every_venv_interpreter_spelling_is_a_repo_local_program() {
        for rel in VENV_PYTHON_RELPATHS {
            let normalized = normalized_program(rel);
            assert_eq!(
                repo_local_program(&normalized),
                Some("python"),
                "{rel} normalizes to {normalized}, which the gate does not recognize"
            );
        }
    }

    /// The install command text and the allowlist must be the same set. They
    /// are rendered from one slice, so this pins the rendering rather than
    /// the values.
    #[test]
    fn the_pytest_install_tail_lists_exactly_the_allowed_packages() {
        let tail = pytest_install_arguments();
        let rendered: Vec<&str> = tail.split(' ').collect();
        assert_eq!(rendered, PYTEST_PACKAGES.to_vec());
        assert!(
            !PYTEST_PACKAGES.is_empty(),
            "an empty set would allow `pip install` with no packages"
        );
    }

    #[test]
    fn the_default_js_provider_is_installable() {
        assert!(JS_COVERAGE_PROVIDERS.contains(&DEFAULT_JS_COVERAGE_PROVIDER));
    }

    /// The generate step writes lcov into the directory the configure step
    /// creates. If these drift, GitPulse builds coverage data into a place its
    /// own scanner never looks.
    #[test]
    fn the_native_lcov_path_is_inside_the_managed_build_directory() {
        let prefix = format!("{NATIVE_COVERAGE_BUILD_DIR}/");
        assert!(
            NATIVE_COVERAGE_LCOV.starts_with(&prefix),
            "{NATIVE_COVERAGE_LCOV} must live inside {NATIVE_COVERAGE_BUILD_DIR}"
        );
        // The build directory is created by the tool, so it must not collide
        // with a name projects use themselves.
        assert_ne!(NATIVE_COVERAGE_BUILD_DIR, "build");
        assert_ne!(NATIVE_COVERAGE_BUILD_DIR, "cmake-build-debug");
    }

    #[test]
    fn the_gcovr_install_tail_lists_exactly_the_allowed_packages() {
        let tail = gcovr_install_arguments();
        let rendered: Vec<&str> = tail.split(' ').collect();
        assert_eq!(rendered, GCOVR_PACKAGES.to_vec());
        assert!(
            !GCOVR_PACKAGES.is_empty(),
            "an empty set would allow `pip install` with no packages"
        );
        assert!(
            GCOVR_PACKAGES.contains(&GCOVR_MODULE),
            "the module GitPulse runs must be one it is allowed to install"
        );
    }
}
