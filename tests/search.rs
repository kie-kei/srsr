use std::{fs, path::Path};

use srsr::{
    ingest::index_collection,
    search::{SearchOptions, search_collection},
};
use tempfile::TempDir;

fn options(query: &str) -> SearchOptions {
    SearchOptions {
        query: query.into(),
        filters: vec![],
        fuzzy: 0,
        fuzzy_prefix: false,
        any: false,
        limit: 20,
        offset: 0,
        facets: vec![],
        stats: vec![],
        explain: false,
        sort: None,
        fields: vec![],
        snippet: true,
    }
}

fn fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("notes");
    let data = dir.path().join("indexes");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("ru.md"), "---\ntitle: Базы знаний\ntags: [машины, Search Engines]\nstatus: Draft\nyear: 2024\nrating: 4.5\nactive: true\nowner:\n  name: Alice Smith\n---\n# Поиск\nМашины помогают людям искать документы.\nA quick brown fox. Café database.\n").unwrap();
    fs::write(source.join("en.MD"), "---\ntitle: Search notes\ntags: [running, Guides]\nstatus: published\nyear: 2025\nrating: 2.5\nactive: false\n---\n# Notes\nRunning wolves walked through forests. A quick red brown fox.\n").unwrap();
    index_collection(&data, "work", &source, &[]).unwrap();
    (dir, source, data)
}

fn paths(data: &Path, query: &str) -> Vec<String> {
    let result = search_collection(data, "work", &options(query)).unwrap();
    let mut paths: Vec<_> = result["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["path"].as_str().unwrap().to_owned())
        .collect();
    paths.sort();
    paths
}

#[test]
fn bilingual_stems_fuzzy_and_exact_metadata_have_distinct_semantics() {
    let (_dir, _source, data) = fixture();
    assert_eq!(paths(&data, "машина"), ["ru.md"]);
    assert_eq!(paths(&data, "walk"), ["en.MD"]);
    assert_eq!(paths(&data, "CAFE"), ["ru.md"]);
    assert_eq!(paths(&data, "meta.tags:машина"), ["ru.md"]);
    assert_eq!(paths(&data, "meta.tags:run"), ["en.MD"]);
    let mut search = options("meta.tags:машна");
    search.fuzzy = 1;
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["total"],
        1
    );
    search.query = "databaze".into();
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["total"],
        1
    );
    search.query = "*".into();
    search.filters = vec!["exact.status:Draft".into()];
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["total"],
        1
    );
    search.filters = vec!["exact.status:draft".into()];
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["total"],
        0
    );
    search.filters = vec!["exact.tags:машина".into()];
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["total"],
        0
    );
    search.filters = vec!["exact.tags:машины".into()];
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["total"],
        1
    );
}

#[test]
fn native_query_language_covers_positions_ranges_sets_and_nested_metadata() {
    let (_dir, _source, data) = fixture();
    assert_eq!(paths(&data, "body:\"quick brown fox\""), ["ru.md"]);
    assert_eq!(
        paths(&data, "body:\"quick brown fox\"~1"),
        ["en.MD", "ru.md"]
    );
    assert_eq!(paths(&data, "body:\"quick bro\"*"), ["ru.md"]);
    assert_eq!(paths(&data, "body:/datab.*/"), ["ru.md"]);
    assert_eq!(paths(&data, "(машина OR walk) -body:forests"), ["ru.md"]);
    assert_eq!(paths(&data, "exact.year:[2024 TO 2024]"), ["ru.md"]);
    assert_eq!(
        paths(&data, "exact.status: IN [Draft published]"),
        ["en.MD", "ru.md"]
    );
    assert_eq!(paths(&data, "exact.owner.name:\"Alice Smith\""), ["ru.md"]);
    assert_eq!(paths(&data, "meta.owner.name:alice"), ["ru.md"]);
    assert_eq!(paths(&data, "exact.owner.name:*"), ["ru.md"]);
    let mut search = options("*");
    search.filters = vec!["exact.active:true".into(), "exact.rating:[4 TO 5]".into()];
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["total"],
        1
    );
    assert!(paths(&data, "машина walk").is_empty());
    search = options("машина walk");
    search.any = true;
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["total"],
        2
    );
    search = options("datab");
    search.fuzzy = 1;
    search.fuzzy_prefix = true;
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["total"],
        1
    );
}

#[test]
fn synchronization_is_atomic_and_collections_do_not_leak() {
    let (dir, source, data) = fixture();
    let other = dir.path().join("private");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("secret.md"), "Secret pineapple").unwrap();
    index_collection(&data, "private", &other, &[]).unwrap();
    assert!(paths(&data, "pineapple").is_empty());
    assert_eq!(
        search_collection(&data, "private", &options("pineapple")).unwrap()["total"],
        1
    );
    let report =
        serde_json::to_value(index_collection(&data, "work", &source, &[]).unwrap()).unwrap();
    assert_eq!(report["unchanged"], 2);
    fs::write(source.join("ru.md"), "# Changed\nNewly invented unicorn").unwrap();
    fs::remove_file(source.join("en.MD")).unwrap();
    fs::write(
        source.join("broken.md"),
        "---\ntags: [unterminated\n---\nBroken",
    )
    .unwrap();
    assert!(index_collection(&data, "work", &source, &[]).is_err());
    assert_eq!(paths(&data, "walk"), ["en.MD"]);
    assert!(paths(&data, "unicorn").is_empty());
    fs::remove_file(source.join("broken.md")).unwrap();
    index_collection(&data, "work", &source, &[]).unwrap();
    assert_eq!(paths(&data, "unicorn"), ["ru.md"]);
    assert!(paths(&data, "walk").is_empty());
    assert!(paths(&data, "машина").is_empty());
    fs::remove_file(source.join("ru.md")).unwrap();
    index_collection(&data, "work", &source, &[]).unwrap();
    assert!(paths(&data, "*").is_empty());
}

#[test]
fn aggregates_cover_all_matches_and_results_keep_original_metadata() {
    let (_dir, _source, data) = fixture();
    let mut search = options("*");
    search.limit = 1;
    search.facets = vec!["status".into(), "tags".into()];
    search.stats = vec!["rating".into()];
    let result = search_collection(&data, "work", &search).unwrap();
    assert_eq!(result["total"], 2);
    assert_eq!(result["hits"].as_array().unwrap().len(), 1);
    let stats = &result["aggregations"]["stats"]["rating"];
    assert_eq!(stats["count"], 2);
    assert_eq!(stats["sum"], 7.0);
    assert_eq!(stats["avg"], 3.5);
    let buckets = result["aggregations"]["facets"]["status"]["buckets"]
        .as_array()
        .unwrap();
    assert_eq!(buckets.len(), 2);
    assert!(
        buckets
            .iter()
            .any(|bucket| bucket["key"] == "Draft" && bucket["doc_count"] == 1)
    );
    search = options("body:машина");
    search.explain = true;
    let result = search_collection(&data, "work", &search).unwrap();
    let hit = &result["hits"][0];
    assert_eq!(hit["metadata"]["owner"]["name"], "Alice Smith");
    assert_eq!(hit["metadata"]["active"], true);
    assert!(hit["snippet"].as_str().unwrap().contains("<b>"));
    assert!(hit.get("explanation").is_some());
    search = options("*");
    search.offset = 2;
    assert_eq!(
        search_collection(&data, "work", &search).unwrap()["hits"],
        serde_json::json!([])
    );
}

#[test]
fn invalid_queries_and_collection_paths_fail_closed() {
    let (_dir, _source, data) = fixture();
    for name in ["../work", "", "work/../private", "/tmp/work"] {
        assert!(search_collection(&data, name, &options("*")).is_err());
    }
    assert!(search_collection(&data, "missing", &options("*")).is_err());
    assert!(!data.join("missing").exists());
    for query in ["", "body:\"unfinished"] {
        assert!(
            search_collection(&data, "work", &options(query)).is_err(),
            "accepted {query:?}"
        );
    }
    let mut search = options("*");
    search.filters = vec!["exact.year:[broken".into()];
    assert!(search_collection(&data, "work", &search).is_err());
    search = options("*");
    search.limit = 0;
    assert!(search_collection(&data, "work", &search).is_err());
    search.limit = 1001;
    assert!(search_collection(&data, "work", &search).is_err());
    search = options("*");
    search.offset = 100_001;
    assert!(search_collection(&data, "work", &search).is_err());
    search = options("*");
    search.fuzzy = 3;
    assert!(search_collection(&data, "work", &search).is_err());
    search = options("*");
    search.fuzzy_prefix = true;
    assert!(search_collection(&data, "work", &search).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(data.join("work"), data.join("alias")).unwrap();
        assert!(search_collection(&data, "alias", &options("*")).is_err());
    }
}

#[test]
fn sort_orders_by_frontmatter_path_and_type_with_missing_last() {
    let (_dir, _source, data) = fixture();
    let mut search = options("*");
    search.sort = Some("year:desc".into());
    search.fields = vec!["path".into(), "score".into()];
    let result = search_collection(&data, "work", &search).unwrap();
    let paths: Vec<_> = result["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["en.MD", "ru.md"], "newest year first");
    assert!(result["hits"][0].get("title").is_none());
    assert!(result["hits"][0]["score"].as_f64().is_some());

    search.sort = Some("path".into());
    let result = search_collection(&data, "work", &search).unwrap();
    let paths: Vec<_> = result["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["en.MD", "ru.md"]);

    search.sort = Some("owner.name".into());
    let result = search_collection(&data, "work", &search).unwrap();
    // en.MD has no owner, so it sorts after the typed value in ru.md.
    assert_eq!(result["hits"][0]["path"], "ru.md");

    search.sort = Some("year:sideways".into());
    assert!(search_collection(&data, "work", &search).is_err());
    search.sort = Some("year".into());
    search.fields = vec!["nosuch".into()];
    assert!(search_collection(&data, "work", &search).is_err());
}

#[test]
fn field_selection_and_snippet_flag_shape_hits() {
    let (_dir, _source, data) = fixture();
    let mut search = options("машина");
    search.fields = vec!["path".into()];
    let result = search_collection(&data, "work", &search).unwrap();
    let hit = &result["hits"][0];
    assert_eq!(hit.as_object().unwrap().len(), 1);
    assert!(hit.get("path").is_some());

    let mut search = options("машина");
    search.snippet = false;
    let result = search_collection(&data, "work", &search).unwrap();
    let hit = &result["hits"][0];
    assert!(hit.get("snippet").is_none());
    assert!(hit.get("title").is_some());
}

#[test]
fn facet_size_limits_buckets_and_validation_stays_strict() {
    let (_dir, _source, data) = fixture();
    let mut search = options("*");
    search.facets = vec!["tags:1".into()];
    let result = search_collection(&data, "work", &search).unwrap();
    let buckets = result["aggregations"]["facets"]["tags"]["buckets"]
        .as_array()
        .unwrap();
    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0]["doc_count"], 1);
    search.facets = vec!["tags:0".into()];
    assert!(search_collection(&data, "work", &search).is_err());
    search.facets = vec!["tags:101".into()];
    assert!(search_collection(&data, "work", &search).is_err());
}

#[test]
fn excludes_skip_matching_files_and_can_be_relaxed() {
    let (dir, source, data) = fixture();
    fs::create_dir_all(source.join("drafts")).unwrap();
    fs::write(source.join("drafts/wip.md"), "# Draft\nwipmachine").unwrap();
    fs::write(source.join("skip.skip.md"), "# Skip\nskipmachine").unwrap();
    index_collection(
        &data,
        "work",
        &source,
        &["drafts".to_owned(), "*.skip.md".to_owned()],
    )
    .unwrap();
    assert_eq!(paths(&data, "wipmachine"), Vec::<String>::new());
    assert_eq!(paths(&data, "skipmachine"), Vec::<String>::new());
    assert_eq!(paths(&data, "машина"), ["ru.md"]);
    index_collection(&data, "work", &source, &[]).unwrap();
    assert_eq!(paths(&data, "wipmachine"), ["drafts/wip.md"]);
    assert_eq!(paths(&data, "skipmachine"), ["skip.skip.md"]);
    assert!(
        index_collection(&data, "work", &source, &["[".to_owned()]).is_err(),
        "invalid glob rejected"
    );
    let meta: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(dir.path().join("indexes").join("work").join("srsr.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(meta["schema_version"], 1);
    assert_eq!(meta["documents"], 4);
    assert!(meta["indexed_at"].as_str().unwrap().ends_with('Z'));
}
