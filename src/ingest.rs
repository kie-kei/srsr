use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use globset::{GlobBuilder, GlobMatcher};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Serialize;
use serde_json::{Map, Value};
use tantivy::{
    DocAddress, Index, TantivyDocument, Term,
    directory::MmapDirectory,
    schema::{OwnedValue, Value as TantivyValue},
};
use walkdir::WalkDir;

use crate::{
    CollectionMeta, SCHEMA_VERSION, collection_path, open_collection, register, schema,
    write_collection_meta,
};

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

pub fn index_collection(
    root: &Path,
    collection: &str,
    source: &Path,
    excludes: &[String],
) -> Result<IndexReport> {
    let path = collection_path(root, collection)?;
    let matchers = compile_excludes(excludes)?;
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
        if is_excluded(&relative, &matchers) {
            continue;
        }
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
        Ok(report) => {
            write_collection_meta(
                &path,
                &CollectionMeta {
                    schema_version: SCHEMA_VERSION,
                    source: source.display().to_string(),
                    indexed_at: rfc3339_utc_now()?,
                    documents: (report.added + report.updated + report.unchanged) as u64,
                },
            );
            Ok(report)
        }
        Err(error) => {
            writer
                .rollback()
                .context(format!("indexing failed ({error:#}); rollback also failed"))?;
            Err(error)
        }
    }
}

fn compile_excludes(excludes: &[String]) -> Result<Vec<GlobMatcher>> {
    excludes
        .iter()
        .map(|pattern| {
            ensure!(
                !pattern.trim().is_empty(),
                "exclude pattern must not be empty"
            );
            GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .with_context(|| format!("invalid exclude pattern {pattern:?}"))
                .map(|glob| glob.compile_matcher())
        })
        .collect()
}

/// A file is excluded when its relative path matches a pattern, when any path
/// component matches (gitignore-style, so `.obsidian` and `*.skip.md` work at
/// any depth), or when an ancestor directory matches the pattern itself.
fn is_excluded(relative: &str, matchers: &[GlobMatcher]) -> bool {
    for matcher in matchers {
        if matcher.is_match(relative)
            || relative
                .split('/')
                .any(|component| matcher.is_match(component))
        {
            return true;
        }
        let mut ancestor = relative;
        while let Some(index) = ancestor.rfind('/') {
            ancestor = &ancestor[..index];
            if matcher.is_match(ancestor) {
                return true;
            }
        }
    }
    false
}

fn rfc3339_utc_now() -> Result<String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs() as i64;
    Ok(format_unix_seconds(seconds))
}

fn format_unix_seconds(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

/// Howard Hinnant's civil-from-days conversion for the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_pointer = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_pointer + 2) / 5 + 1) as u32;
    let month = if month_pointer < 10 {
        month_pointer + 3
    } else {
        month_pointer - 9
    } as u32;
    (year + i64::from(month <= 2), month, day)
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
            let yaml: serde_yaml_ng::Value =
                serde_yaml_ng::from_str(yaml).context("invalid YAML frontmatter")?;
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

fn yaml_to_json(value: serde_yaml_ng::Value) -> Result<Value> {
    use serde_yaml_ng::Value as Yaml;
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

    #[test]
    fn exclusion_matches_paths_components_and_ancestor_directories() {
        let matchers = compile_excludes(&[
            ".obsidian".into(),
            "drafts/**".into(),
            "*.skip.md".into(),
            "private/nested".into(),
        ])
        .unwrap();
        assert!(is_excluded(".obsidian/app.json", &matchers));
        assert!(is_excluded("deep/dir/.obsidian/x", &matchers));
        assert!(is_excluded("drafts/wip.md", &matchers));
        assert!(is_excluded("notes/inner.skip.md", &matchers));
        assert!(is_excluded("private/nested/deep/file.md", &matchers));
        assert!(!is_excluded("notes/keep.md", &matchers));
        assert!(!is_excluded("notdrafts/wip.md", &matchers));
        assert!(!is_excluded("private/file.md", &matchers));
    }

    #[test]
    fn timestamps_render_rfc3339_utc() {
        assert_eq!(format_unix_seconds(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_unix_seconds(1_735_689_600), "2025-01-01T00:00:00Z");
        assert_eq!(format_unix_seconds(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(format_unix_seconds(86_399), "1970-01-01T23:59:59Z");
    }

    use proptest::prelude::*;

    fn arbitrary_text() -> BoxedStrategy<String> {
        let pieces = proptest::sample::select(vec![
            "---".to_owned(),
            "---\n".to_owned(),
            "---\r\n".to_owned(),
            "\u{feff}".to_owned(),
            "title: x\n".to_owned(),
            "key: [1, 2\n".to_owned(),
            "# comment\n".to_owned(),
            "null\n".to_owned(),
            "- item\n".to_owned(),
            "ключ: значение\n".to_owned(),
            "a: !tag v\n".to_owned(),
            ":\n".to_owned(),
            "\n".to_owned(),
            "\r".to_owned(),
            "\u{2028}".to_owned(),
            "trailing   \n".to_owned(),
            "…\n".to_owned(),
        ]);
        prop_oneof![
            proptest::collection::vec(pieces, 0..48).prop_map(|pieces| pieces.concat()),
            proptest::collection::vec(proptest::char::any(), 0..256)
                .prop_map(|chars| chars.into_iter().collect()),
        ]
        .boxed()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn frontmatter_parser_never_panics_on_arbitrary_text(text in arbitrary_text()) {
            let _ = parse_document(&text, Path::new("file.md"));
        }
    }
}
