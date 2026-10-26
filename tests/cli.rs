use std::{fs, path::Path, process::Command};

use serde_json::Value;

struct Output {
    success: bool,
    stdout: String,
    stderr: String,
}

fn run_raw(home: &Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_srsr"))
        .env("HOME", home)
        .env("USERPROFILE", home)
        .current_dir(home)
        .args(args)
        .output()
        .unwrap();
    Output {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn run(home: &Path, args: &[&str]) -> Value {
    let output = run_raw(home, args);
    assert!(output.success, "{}", output.stderr);
    serde_json::from_str(&output.stdout).unwrap()
}

fn workspace() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let source = temp.path().join("notes");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("a.md"),
        "---\ntitle: Alpha\nyear: 2023\n---\n# Alpha\nalpha body",
    )
    .unwrap();
    fs::write(
        source.join("b.md"),
        "---\ntitle: Beta\nyear: 2025\n---\n# Beta\nbeta body",
    )
    .unwrap();
    (temp, home, source)
}

#[test]
fn collections_list_reports_counts_metadata_and_foreign_directories() {
    let (_temp, home, source) = workspace();
    run(&home, &["index", "-c", "alpha", source.to_str().unwrap()]);
    run(&home, &["index", "-c", "beta", source.to_str().unwrap()]);
    fs::create_dir(home.join(".srsr").join("not-an-index")).unwrap();
    let listed = run(&home, &["collections"]);
    let collections = listed["collections"].as_array().unwrap();
    assert_eq!(collections.len(), 3);
    let alpha = collections
        .iter()
        .find(|entry| entry["name"] == "alpha")
        .unwrap();
    assert_eq!(alpha["documents"], 2);
    assert_eq!(alpha["source"], source.to_str().unwrap());
    assert!(alpha["indexed_at"].as_str().unwrap().ends_with('Z'));
    let foreign = collections
        .iter()
        .find(|entry| entry["name"] == "not-an-index")
        .unwrap();
    assert!(foreign["error"].as_str().is_some());
    assert!(foreign.get("documents").is_none());
    // An untouched data directory lists nothing without failing.
    let fresh = tempfile::tempdir().unwrap();
    let fresh_home = fresh.path().join("home");
    fs::create_dir(&fresh_home).unwrap();
    assert_eq!(
        run(&fresh_home, &["collections"])["collections"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn delete_requires_confirmation_and_only_removes_the_index() {
    let (_temp, home, source) = workspace();
    run(&home, &["index", "-c", "work", source.to_str().unwrap()]);
    let refused = run_raw(&home, &["delete", "-c", "work"]);
    assert!(!refused.success);
    assert!(home.join(".srsr/work/meta.json").is_file());
    assert!(source.join("a.md").is_file());
    let deleted = run(&home, &["delete", "-c", "work", "--yes"]);
    assert_eq!(deleted["deleted"], "work");
    assert!(!home.join(".srsr/work").exists());
    assert!(source.join("a.md").is_file(), "sources survive deletion");
    assert!(!run_raw(&home, &["search", "-c", "work", "*"]).success);
    let missing = run_raw(&home, &["delete", "-c", "work", "--yes"]);
    assert!(!missing.success);
    fs::create_dir(home.join(".srsr/keep-me")).unwrap();
    fs::write(home.join(".srsr/keep-me/other.txt"), "data").unwrap();
    assert!(!run_raw(&home, &["delete", "-c", "keep-me", "--yes"]).success);
    assert!(home.join(".srsr/keep-me/other.txt").is_file());
}

#[test]
fn search_jsonl_streams_hits_then_a_total_envelope() {
    let (_temp, home, source) = workspace();
    run(&home, &["index", "-c", "work", source.to_str().unwrap()]);
    let output = run_raw(
        &home,
        &[
            "search",
            "-c",
            "work",
            "*",
            "--sort",
            "year:desc",
            "--no-snippet",
            "--jsonl",
        ],
    );
    assert!(output.success, "{}", output.stderr);
    let mut lines = output.stdout.lines();
    let first: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(first["path"], "b.md");
    assert!(first.get("snippet").is_none());
    let second: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(second["path"], "a.md");
    let envelope: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(envelope["total"], 2);
    assert!(envelope.get("hits").is_none());
    assert!(lines.next().is_none());
    // Facets ride along on the envelope line.
    let output = run_raw(
        &home,
        &[
            "search", "-c", "work", "*", "--facet", "year", "--fields", "path", "--jsonl",
        ],
    );
    let envelope: Value = serde_json::from_str(output.stdout.lines().last().unwrap()).unwrap();
    assert!(
        envelope["aggregations"]["facets"]["year"]["buckets"]
            .as_array()
            .is_some()
    );
}

#[test]
fn completions_emit_shell_scripts() {
    let (_temp, home, _source) = workspace();
    for shell in ["bash", "zsh", "fish"] {
        let output = run_raw(&home, &["completions", shell]);
        assert!(output.success, "{}", output.stderr);
        assert!(
            output.stdout.contains("srsr"),
            "{shell} completions mention the binary"
        );
        assert!(!output.stdout.is_empty());
    }
    assert!(!run_raw(&home, &["completions", "tcsh"]).success);
}

#[test]
fn newer_schema_metadata_fails_closed() {
    let (_temp, home, source) = workspace();
    run(&home, &["index", "-c", "work", source.to_str().unwrap()]);
    let meta_file = home.join(".srsr").join("work").join("srsr.json");
    let meta: Value = serde_json::from_str(&fs::read_to_string(&meta_file).unwrap()).unwrap();
    let mut future = meta.clone();
    future["schema_version"] = serde_json::json!(u32::MAX);
    fs::write(&meta_file, serde_json::to_string_pretty(&future).unwrap()).unwrap();
    let refused = run_raw(&home, &["search", "-c", "work", "*"]);
    assert!(!refused.success);
    assert!(refused.stderr.contains("upgrade srsr"));
}
