use srsr::{
    ingest::index_collection,
    search::{SearchOptions, search_collection},
};
use std::fs;

fn query(text: &str) -> SearchOptions {
    SearchOptions {
        query: text.into(),
        filters: vec![],
        fuzzy: 0,
        fuzzy_prefix: false,
        any: false,
        limit: 20,
        offset: 0,
        facets: vec![],
        stats: vec![],
        explain: false,
    }
}

#[test]
fn frontmatter_types_bom_crlf_and_literal_dotted_keys_survive_indexing() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let data = dir.path().join("data");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("note.md"), "\u{feff}---\r\ntitle: Types\r\ndate: '2025-01-15'\r\nnumber: 2025\r\nstring: '2025'\r\nempty: null\r\nbuild.version: stable\r\nitems: [one, two]\r\n---\r\n# Content\r\n`visible_code` and [visible link](https://example.com/hidden_url).\r\nA data**base**.\r\n").unwrap();
    index_collection(&data, "types", &source).unwrap();
    for text in [
        "exact.date:[2025-01-01 TO 2025-12-31]",
        "exact.build\\.version:stable",
        "exact.items:two",
        "body:visible_code",
        "body:visible",
        "body:database",
    ] {
        let result = search_collection(&data, "types", &query(text)).unwrap();
        assert_eq!(result["total"], 1, "query {text}");
    }
    let result = search_collection(&data, "types", &query("*")).unwrap();
    let meta = &result["hits"][0]["metadata"];
    assert_eq!(meta["date"], "2025-01-15");
    assert_eq!(meta["number"], 2025);
    assert_eq!(meta["string"], "2025");
    assert!(meta["empty"].is_null());
    assert_eq!(
        search_collection(&data, "types", &query("exact.empty:*")).unwrap()["total"],
        0
    );
    assert_eq!(
        search_collection(&data, "types", &query("hidden_url")).unwrap()["total"],
        0
    );
    for invalid in [
        "---\n- not\n- an object\n---\nText",
        "---\n3: invalid key\n---\nText",
        "---\nnull\n---\nText",
        "---\nfield: .nan\n---\nText",
    ] {
        fs::write(source.join("bad.md"), invalid).unwrap();
        assert!(
            index_collection(&data, "types", &source).is_err(),
            "accepted {invalid}"
        );
        assert_eq!(
            search_collection(&data, "types", &query("*")).unwrap()["total"],
            1
        );
    }
}

#[test]
fn empty_frontmatter_and_leading_markdown_rules_remain_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let data = dir.path().join("data");
    fs::create_dir(&source).unwrap();
    let cases = [
        (
            "empty.md",
            "\u{feff}---\r\n---\r\n# Empty\r\nemptybody",
            "Empty",
            "emptybody",
        ),
        (
            "comments.md",
            "---\n  # Metadata to be added\n\n---\n# Comments\ncommentbody",
            "Comments",
            "commentbody",
        ),
        ("rule.md", "---\n\n# Rule\nrulebody", "Rule", "rulebody"),
        (
            "unclosed.md",
            "---\nfield: visiblevalue\n\n# Unclosed\nunclosedbody",
            "Unclosed",
            "visiblevalue",
        ),
    ];
    for (filename, text, _, _) in &cases {
        fs::write(source.join(filename), text).unwrap();
    }
    index_collection(&data, "notes", &source).unwrap();
    for (filename, _, title, term) in cases {
        let result = search_collection(&data, "notes", &query(&format!("body:{term}"))).unwrap();
        assert_eq!(result["total"], 1, "{filename}");
        assert_eq!(result["hits"][0]["path"], filename);
        assert_eq!(result["hits"][0]["title"], title);
        assert_eq!(result["hits"][0]["metadata"], serde_json::json!({}));
    }
}
