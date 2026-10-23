pub mod ingest;
pub mod search;

use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};
use tantivy::{Index, schema::*, tokenizer::*};

pub const ANALYZER: &str = "ru_en";

#[derive(Clone, Copy)]
pub struct Fields {
    pub path: Field,
    pub title: Field,
    pub body: Field,
    pub meta: Field,
    pub exact: Field,
}

pub fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let text_indexing = TextFieldIndexing::default()
        .set_tokenizer(ANALYZER)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let text = TextOptions::default()
        .set_stored()
        .set_indexing_options(text_indexing.clone());
    let path = builder.add_text_field("path", STRING | STORED);
    let title = builder.add_text_field("title", text.clone());
    let body = builder.add_text_field("body", text);
    let meta = builder.add_json_field(
        "meta",
        JsonObjectOptions::default()
            .set_stored()
            .set_fast("raw")
            .set_indexing_options(text_indexing),
    );
    let exact = builder.add_json_field(
        "exact",
        JsonObjectOptions::default()
            .set_fast("raw")
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("raw")
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            ),
    );
    (
        builder.build(),
        Fields {
            path,
            title,
            body,
            meta,
            exact,
        },
    )
}

pub fn register(index: &Index) {
    let analyzer = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(80))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .filter(Stemmer::new(Language::Russian))
        .filter(Stemmer::new(Language::English))
        .build();
    index.tokenizers().register(ANALYZER, analyzer);
}

pub fn collection_path(root: &Path, name: &str) -> Result<PathBuf> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "collection must contain only ASCII letters, digits, '-' or '_' (1..128 bytes)"
    );
    let path = root.join(name);
    if path.exists() {
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "collection directory must not be a symlink"
        );
    }
    Ok(path)
}

pub fn open_collection(root: &Path, name: &str) -> Result<(Index, Fields)> {
    let path = collection_path(root, name)?;
    let index = Index::open_in_dir(&path)
        .with_context(|| format!("cannot open collection {name:?}; index it first"))?;
    let (expected, fields) = schema();
    ensure!(
        index.schema() == expected,
        "unsupported index schema; use a new data directory and reindex"
    );
    register(&index);
    Ok((index, fields))
}
