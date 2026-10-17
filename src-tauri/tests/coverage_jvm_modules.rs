//! Multi-module Maven and Gradle builds write each module's JaCoCo report under
//! that module (`<module>/target/site/jacoco/jacoco.xml`), never at the root.
//! The scanner looked only at the root locations, so a multi-module build read
//! as "no coverage report" while every module had one.

mod common;

use gitpulse_lib::analyzer::coverage::CoverageScanner;
use gitpulse_lib::procguard::LockedSpawn;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn git_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .status_locked()
        .expect("git init");
    assert!(status.success());
    common::trust_repo(dir.path());
    dir
}

fn write(dir: &Path, rel: &str, content: &str) {
    let dest = dir.join(rel);
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    fs::write(dest, content).unwrap();
}

fn jacoco(package: &str, file: &str, line: u32) -> String {
    format!(
        r#"<report><package name="{package}"><sourcefile name="{file}"><line nr="{line}" ci="2" mi="0"/></sourcefile></package></report>"#
    )
}

fn source(lines: usize) -> String {
    "class X {}\n".repeat(lines)
}

#[test]
fn each_maven_module_report_is_found_and_resolved_against_its_own_module() {
    let repo = git_repo();
    for module in ["core", "web"] {
        write(repo.path(), &format!("{module}/pom.xml"), "<project/>\n");
        write(
            repo.path(),
            &format!("{module}/src/main/java/com/acme/Same.java"),
            &source(3),
        );
        write(
            repo.path(),
            &format!("{module}/target/site/jacoco/jacoco.xml"),
            &jacoco(
                "com/acme",
                "Same.java",
                if module == "core" { 1 } else { 2 },
            ),
        );
    }
    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    let artifacts: Vec<&str> = report.artifacts.iter().map(|a| a.path.as_str()).collect();
    assert!(
        artifacts.contains(&"core/target/site/jacoco/jacoco.xml")
            && artifacts.contains(&"web/target/site/jacoco/jacoco.xml"),
        "module reports were not discovered: {artifacts:?}"
    );
    let files: Vec<&str> = report.files.iter().map(|f| f.path.as_str()).collect();
    assert!(
        files.contains(&"core/src/main/java/com/acme/Same.java")
            && files.contains(&"web/src/main/java/com/acme/Same.java"),
        "same-named classes must land on their own module's source: {files:?}"
    );
    assert_eq!(report.overall.lines_found, 2);
}

#[test]
fn a_gradle_module_report_is_found_under_its_build_directory() {
    let repo = git_repo();
    write(repo.path(), "app/build.gradle.kts", "plugins {}\n");
    write(
        repo.path(),
        "app/src/main/kotlin/com/acme/Main.kt",
        &source(2),
    );
    write(
        repo.path(),
        "app/build/reports/jacoco/test/jacocoTestReport.xml",
        &jacoco("com/acme", "Main.kt", 1),
    );
    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    assert!(
        report
            .files
            .iter()
            .any(|f| f.path == "app/src/main/kotlin/com/acme/Main.kt"),
        "{:?} / {:?}",
        report.artifacts,
        report.files
    );
}

#[test]
fn a_build_file_without_a_report_adds_no_artifact_and_no_error() {
    let repo = git_repo();
    write(repo.path(), "lib/pom.xml", "<project/>\n");
    write(repo.path(), "lib/src/main/java/com/acme/A.java", &source(1));
    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    assert!(report.artifacts.is_empty(), "{:?}", report.artifacts);
    assert!(report.files.is_empty());
}

#[test]
fn a_vendored_modules_build_file_is_not_probed() {
    let repo = git_repo();
    write(repo.path(), "vendor/dep/pom.xml", "<project/>\n");
    write(repo.path(), "src/main/java/com/acme/A.java", &source(1));
    write(
        repo.path(),
        "vendor/dep/target/site/jacoco/jacoco.xml",
        &jacoco("com/acme", "A.java", 1),
    );
    let report = CoverageScanner::scan(repo.path().to_str().unwrap()).expect("scan");
    assert!(
        !report
            .artifacts
            .iter()
            .any(|a| a.path.starts_with("vendor/")),
        "{:?}",
        report.artifacts
    );
}
