use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, bail, ensure};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Serialize;
use serde_json::{Map, Value};
use tantivy::{
    DocAddress, Index, TantivyDocument, Term,
    directory::MmapDirectory,
    schema::{OwnedValue, Value as TantivyValue},
};
use walkdir::WalkDir;

use crate::{collection_path, open_collection, register, schema};

#[derive(Debug, Default, Serialize)]
pub struct IndexReport {
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub unchanged: usize,
}

struct ParsedDocument {
    title: String,
    body: String,
    meta: Map<String, Value>,
}

pub fn index_collection(root: &Path, collection: &str, source: &Path) -> Result<IndexReport> {
    let path = collection_path(root, collection)?;
    let source_info = fs::symlink_metadata(source)
        .with_context(|| format!("cannot inspect source {}", source.display()))?;
    ensure!(
        !source_info.file_type().is_symlink(),
        "source directory must not be a symlink"
    );
    ensure!(source_info.is_dir(), "source must be a directory");

    // shortcut: hold the parsed snapshot in memory; use two passes if collections outgrow RAM.
    let mut documents = BTreeMap::new();
    for entry in WalkDir::new(source).follow_links(false) {
        let entry = entry.with_context(|| format!("cannot walk source {}", source.display()))?;
        if !entry.file_type().is_file()
            || !entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let relative = entry.path().strip_prefix(source)?;
        let relative = relative
            .to_str()
            .context("Markdown paths must be valid UTF-8")?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let text = fs::read_to_string(entry.path())
            .with_context(|| format!("cannot read {}", entry.path().display()))?;
        let parsed = parse_document(&text, entry.path())
            .with_context(|| format!("cannot parse {}", entry.path().display()))?;
        documents.insert(relative, parsed);
    }

    fs::create_dir_all(&path).with_context(|| format!("cannot create {}", path.display()))?;
    let directory = MmapDirectory::open(&path)?;
    let (index, fields) = if Index::exists(&directory)? {
        open_collection(root, collection)?
    } else {
        let (schema, fields) = schema();
        let index = Index::create_in_dir(&path, schema)?;
        register(&index);
        (index, fields)
    };
    // Acquire the exclusive writer before reading the old snapshot.
    let mut writer = index.writer::<TantivyDocument>(50_000_000)?;
    let result = (|| -> Result<IndexReport> {
        let reader = index.reader()?;
        let searcher = reader.searcher();
        let mut existing = BTreeMap::new();
        for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
            for doc_id in segment.doc_ids_alive() {
                let address = DocAddress::new(segment_ord as u32, doc_id);
                let doc: TantivyDocument = searcher.doc(address)?;
                let relative = doc
                    .get_first(fields.path)
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .context("stored document has no path")?;
                ensure!(
                    existing.insert(relative, address).is_none(),
                    "index contains duplicate document paths"
                );
            }
        }
        let mut report = IndexReport::default();
        for (relative, parsed) in documents {
            if let Some(address) = existing.remove(&relative) {
                let old: TantivyDocument = searcher.doc(address)?;
                if old.get_first(fields.title).and_then(|value| value.as_str())
                    == Some(parsed.title.as_str())
                    && old.get_first(fields.body).and_then(|value| value.as_str())
                        == Some(parsed.body.as_str())
                    && old
                        .get_first(fields.meta)
                        .map(|value| serde_json::to_value(OwnedValue::from(value)))
                        .transpose()?
                        .as_ref()
                        .and_then(Value::as_object)
                        == Some(&parsed.meta)
                {
                    report.unchanged += 1;
                    continue;
                }
                writer.delete_term(Term::from_field_text(fields.path, &relative));
                report.updated += 1;
            } else {
                report.added += 1;
            }
            let meta: BTreeMap<String, OwnedValue> =
                serde_json::from_value(Value::Object(parsed.meta))?;
            let mut doc = TantivyDocument::default();
            doc.add_text(fields.path, relative);
            doc.add_text(fields.title, parsed.title);
            doc.add_text(fields.body, parsed.body);
            doc.add_object(fields.meta, meta.clone());
            doc.add_object(fields.exact, meta);
            writer.add_document(doc)?;
        }
        for relative in existing.keys() {
            writer.delete_term(Term::from_field_text(fields.path, relative));
            report.deleted += 1;
        }
        writer.commit()?;
        Ok(report)
    })();
    match result {
        Ok(report) => Ok(report),
        Err(error) => {
            writer
                .rollback()
                .context(format!("indexing failed ({error:#}); rollback also failed"))?;
            Err(error)
        }
    }
}

fn parse_document(text: &str, path: &Path) -> Result<ParsedDocument> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    let first = lines.next().unwrap_or("");
    let mut frontmatter = None;
    if first.trim_end_matches(['\r', '\n']) == "---" {
        let start = first.len();
        let mut end = start;
        for line in lines {
            if line.trim_end_matches(['\r', '\n']) == "---" {
                frontmatter = Some((&text[start..end], &text[end + line.len()..]));
                break;
            }
            end += line.len();
        }
    }
    let (meta, markdown) = if let Some((yaml, markdown)) = frontmatter {
        let meta = if yaml
            .lines()
            .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
        {
            Map::new()
        } else {
            let yaml: serde_yaml::Value =
                serde_yaml::from_str(yaml).context("invalid YAML frontmatter")?;
            let Value::Object(meta) = yaml_to_json(yaml)? else {
                bail!("frontmatter must be an object with string keys");
            };
            meta
        };
        (meta, markdown)
    } else {
        (Map::new(), text)
    };
    let mut body = String::new();
    let mut heading = None;
    let mut in_heading = false;
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) if heading.is_none() => {
                heading = Some(String::new());
                in_heading = true;
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) => {
                in_heading = false;
                body.push(' ');
            }
            Event::Text(text) | Event::Code(text) => {
                body.push_str(&text);
                if in_heading {
                    let title = heading.as_mut().expect("heading initialized above");
                    title.push_str(&text);
                }
            }
            Event::End(
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Superscript
                | TagEnd::Subscript
                | TagEnd::Link
                | TagEnd::Image,
            ) => {}
            Event::SoftBreak | Event::HardBreak | Event::Rule | Event::End(_) => {
                body.push(' ');
                if in_heading {
                    heading
                        .as_mut()
                        .expect("heading initialized above")
                        .push(' ');
                }
            }
            _ => {}
        }
    }
    let title = if let Some(title) = meta.get("title").and_then(Value::as_str) {
        title.to_owned()
    } else if let Some(title) = heading {
        title.trim().to_owned()
    } else {
        path.file_name()
            .and_then(|name| name.to_str())
            .context("Markdown filename must be valid UTF-8")?
            .to_owned()
    };
    Ok(ParsedDocument {
        title,
        body: body.trim().to_owned(),
        meta,
    })
}

fn yaml_to_json(value: serde_yaml::Value) -> Result<Value> {
    use serde_yaml::Value as Yaml;
    Ok(match value {
        Yaml::Null => Value::Null,
        Yaml::Bool(value) => Value::Bool(value),
        Yaml::String(value) => Value::String(value),
        Yaml::Number(value) => {
            if let Some(value) = value.as_u64() {
                Value::from(value)
            } else if let Some(value) = value.as_i64() {
                Value::from(value)
            } else {
                let number = value
                    .as_f64()
                    .and_then(serde_json::Number::from_f64)
                    .context("frontmatter numbers must be finite JSON numbers")?;
                Value::Number(number)
            }
        }
        Yaml::Sequence(values) => Value::Array(
            values
                .into_iter()
                .map(yaml_to_json)
                .collect::<Result<_>>()?,
        ),
        Yaml::Mapping(values) => {
            let mut object = Map::new();
            for (key, value) in values {
                let Yaml::String(key) = key else {
                    bail!("frontmatter object keys must be strings");
                };
                object.insert(key, yaml_to_json(value)?);
            }
            Value::Object(object)
        }
        Yaml::Tagged(_) => bail!("tagged YAML values are not supported in frontmatter"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_bom_crlf_typed_metadata_and_markdown_boundaries() {
        let parsed = parse_document(
            "\u{feff}---\r\ntitle: Документ\r\nnested:\r\n  values: [true, 42, -3, 1.5, null, '007']\r\n---\r\n# Other *title*\r\n\r\nfirst\r\n\r\nsecond `code`\r\n\r\n```rust\r\nlet value = 1;\r\n```\r\n",
            Path::new("file.md"),
        ).unwrap();
        assert_eq!(parsed.title, "Документ");
        assert_eq!(
            parsed.meta["nested"],
            json!({"values": [true, 42, -3, 1.5, null, "007"]})
        );
        let words: Vec<_> = parsed.body.split_whitespace().collect();
        assert!(
            words
                .windows(3)
                .any(|words| words == ["first", "second", "code"])
        );
        assert!(parsed.body.contains("let value = 1;"));
        assert_eq!(
            parse_document("# Привет **мир**\n", Path::new("file.md"))
                .unwrap()
                .title,
            "Привет мир"
        );
        assert_eq!(
            parse_document("body", Path::new("file.md")).unwrap().title,
            "file.md"
        );
    }

    #[test]
    fn rejects_lossy_or_invalid_frontmatter() {
        for text in [
            "---\n- sequence\n---\nbody",
            "---\nvalue: {1: wrong}\n---\nbody",
            "---\nvalue: .nan\n---\nbody",
            "---\nvalue: !custom tagged\n---\nbody",
            "---\nvalue: [broken\n---\nbody",
        ] {
            assert!(
                parse_document(text, Path::new("file.md")).is_err(),
                "accepted {text:?}"
            );
        }
    }
}
