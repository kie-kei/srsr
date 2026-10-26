pub mod ingest;
pub mod search;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tantivy::{Index, directory::MmapDirectory, schema::*, tokenizer::*};

pub const ANALYZER: &str = "ru_en";

/// Bumped whenever the on-disk schema or companion metadata layout changes
/// incompatibly; stored in `srsr.json` beside each collection index.
pub const SCHEMA_VERSION: u32 = 1;
const COLLECTION_META_FILE: &str = "srsr.json";
const TANTIVY_META_FILE: &str = "meta.json";

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
    read_collection_meta(&path)?;
    register(&index);
    Ok((index, fields))
}

/// Companion snapshot of `index` runs, written next to the Tantivy index.
#[derive(Debug, Serialize, Deserialize)]
pub struct CollectionMeta {
    pub schema_version: u32,
    /// Source directory exactly as passed to `index`.
    pub source: String,
    /// RFC 3339 (UTC) time of the last successful synchronization.
    pub indexed_at: String,
    pub documents: u64,
}

fn collection_meta_path(path: &Path) -> PathBuf {
    path.join(COLLECTION_META_FILE)
}

/// Reads `srsr.json` if present; collections created before the file existed open normally.
pub fn read_collection_meta(path: &Path) -> Result<Option<CollectionMeta>> {
    let file = collection_meta_path(path);
    if !file.exists() {
        return Ok(None);
    }
    let meta: CollectionMeta = serde_json::from_str(
        &std::fs::read_to_string(&file)
            .with_context(|| format!("cannot read {}", file.display()))?,
    )
    .with_context(|| format!("cannot parse {}", file.display()))?;
    ensure!(
        meta.schema_version <= SCHEMA_VERSION,
        "collection metadata has schema version {} but this srsr supports {}; upgrade srsr",
        meta.schema_version,
        SCHEMA_VERSION
    );
    Ok(Some(meta))
}

/// Writes `srsr.json` after a committed synchronization; a write failure must not
/// turn a published snapshot into a reported failure, so it warns on stderr.
pub fn write_collection_meta(path: &Path, meta: &CollectionMeta) {
    let file = collection_meta_path(path);
    let content = serde_json::to_vec_pretty(meta).expect("collection metadata serializes");
    if let Err(error) =
        std::fs::write(&file, content).with_context(|| format!("cannot write {}", file.display()))
    {
        eprintln!(
            "warning: snapshot committed but cannot update {}: {error:#}",
            file.display()
        );
    }
}

#[derive(Debug, Serialize)]
pub struct CollectionInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documents: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Lists every subdirectory of the data directory root, reporting per-entry
/// errors instead of aborting the whole listing.
pub fn list_collections(root: &Path) -> Result<Vec<CollectionInfo>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    ensure!(root.is_dir(), "{} is not a directory", root.display());
    let mut names = BTreeMap::new();
    for entry in
        std::fs::read_dir(root).with_context(|| format!("cannot read {}", root.display()))?
    {
        let entry = entry.with_context(|| format!("cannot read entries of {}", root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("cannot inspect {}", entry.path().display()))?;
        if file_type.is_dir() || file_type.is_symlink() {
            names.insert(
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            );
        }
    }
    let mut collections = Vec::with_capacity(names.len());
    for (name, path) in names {
        let info = match collection_status(root, &name, &path) {
            Ok(info) => info,
            Err(error) => CollectionInfo {
                name,
                documents: None,
                indexed_at: None,
                source: None,
                error: Some(format!("{error:#}")),
            },
        };
        collections.push(info);
    }
    Ok(collections)
}

fn collection_status(root: &Path, name: &str, path: &Path) -> Result<CollectionInfo> {
    // collection_path revalidates the name charset and rejects symlinks.
    collection_path(root, name)?;
    let directory = MmapDirectory::open(path)?;
    ensure!(
        Index::exists(&directory)?,
        "{} is not an srsr collection index",
        path.display()
    );
    let index = Index::open_in_dir(path)?;
    let meta = read_collection_meta(path)?;
    let documents = index.reader()?.searcher().num_docs();
    Ok(CollectionInfo {
        name: name.to_owned(),
        documents: Some(documents),
        indexed_at: meta.as_ref().map(|meta| meta.indexed_at.clone()),
        source: meta.map(|meta| meta.source),
        error: None,
    })
}

/// Deletes a collection index directory; source Markdown files are never touched.
pub fn delete_collection(root: &Path, name: &str) -> Result<()> {
    let path = collection_path(root, name)?;
    ensure!(
        path.exists(),
        "collection {name:?} does not exist in {}",
        root.display()
    );
    ensure!(
        path.join(TANTIVY_META_FILE).is_file(),
        "{} is not an srsr collection index; refusing to delete",
        path.display()
    );
    std::fs::remove_dir_all(&path).with_context(|| format!("cannot delete {}", path.display()))?;
    Ok(())
}
