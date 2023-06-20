use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::{error::Error, process::Command};

#[test]
fn test_check() -> Result<(), Box<dyn Error>> {
    Command::cargo_bin("packs")?
        .arg("--project-root")
        .arg("tests/fixtures/simple_app")
        .arg("check")
        .assert()
        .failure()
        .stdout(predicate::str::contains("3 violation(s) detected:"))
        .stdout(predicate::str::contains("dependency: packs/foo/app/services/foo.rb:3 references ::Bar from packs/bar without an explicit dependency in packs/foo/package.yml"))
        .stdout(predicate::str::contains("privacy: packs/foo/app/services/foo.rb:7 references private constant ::Baz from packs/baz"))
        .stdout(predicate::str::contains("privacy: packs/foo/app/services/foo.rb:3 references private constant ::Bar from packs/bar"));
    Ok(())
}

#[test]
fn test_check_with_package_todo_file() -> Result<(), Box<dyn Error>> {
    Command::cargo_bin("packs")?
        .arg("--project-root")
        .arg("tests/fixtures/contains_package_todo")
        .arg("check")
        .assert()
        .success()
        .stdout(predicate::str::contains("No violations detected!"));

    Ok(())
}
