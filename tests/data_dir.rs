use std::{fs, path::Path, process::Command};

use serde_json::Value;

fn run(home: &Path, cwd: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_srsr"))
        .env("HOME", home)
        .env("USERPROFILE", home)
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn collections_follow_home_across_working_directories_and_allow_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home with spaces");
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let source = temp.path().join("notes");
    for path in [&home, &first, &second, &source] {
        fs::create_dir(path).unwrap();
    }
    fs::write(source.join("note.md"), "# Home\nhomeword").unwrap();
    run(
        &home,
        &first,
        &["index", "-c", "work", source.to_str().unwrap()],
    );
    let found = run(&home, &second, &["search", "-c", "work", "homeword"]);
    assert_eq!(found["total"], 1);
    assert_eq!(found["hits"][0]["path"], "note.md");
    assert!(home.join(".srsr/work/meta.json").is_file());
    assert!(!first.join(".srsr").exists());
    assert!(!second.join(".srsr").exists());

    fs::write(source.join("note.md"), "# Override\noverrideword").unwrap();
    run(
        &home,
        &second,
        &[
            "--data-dir",
            "custom-indexes",
            "index",
            "-c",
            "work",
            source.to_str().unwrap(),
        ],
    );
    assert!(second.join("custom-indexes/work/meta.json").is_file());
    assert_eq!(
        run(&home, &first, &["search", "-c", "work", "homeword"])["total"],
        1
    );
    assert_eq!(
        run(
            &home,
            &second,
            &[
                "--data-dir",
                "custom-indexes",
                "search",
                "-c",
                "work",
                "homeword"
            ]
        )["total"],
        0
    );
    assert_eq!(
        run(
            &home,
            &second,
            &[
                "--data-dir",
                "custom-indexes",
                "search",
                "-c",
                "work",
                "overrideword"
            ]
        )["total"],
        1
    );
}

#[test]
fn invalid_home_never_silently_uses_the_working_directory() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("notes");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("note.md"), "# Explicit\nexplicitword").unwrap();
    let invalid_home = Path::new("relative-home");
    let output = Command::new(env!("CARGO_BIN_EXE_srsr"))
        .env("HOME", invalid_home)
        .env("USERPROFILE", invalid_home)
        .current_dir(temp.path())
        .args(["index", "-c", "work", source.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!temp.path().join("relative-home").exists());
    assert!(!temp.path().join(".srsr").exists());
    run(
        invalid_home,
        temp.path(),
        &[
            "--data-dir",
            "explicit-indexes",
            "index",
            "-c",
            "work",
            source.to_str().unwrap(),
        ],
    );
    assert_eq!(
        run(
            invalid_home,
            temp.path(),
            &[
                "--data-dir",
                "explicit-indexes",
                "search",
                "-c",
                "work",
                "explicitword"
            ]
        )["total"],
        1
    );
}
