use std::{cmp::Ordering, path::Path};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use tantivy::{
    DocAddress, TantivyDocument,
    aggregation::{
        AggregationCollector,
        agg_req::{Aggregation, AggregationVariants, Aggregations},
        bucket::TermsAggregation,
        metric::StatsAggregation,
    },
    collector::{Collector, Count, SegmentCollector, TopDocs},
    query::{BooleanQuery, ConstScoreQuery, Occur, Query, QueryParser},
    schema::{OwnedValue, Value as TantivyValue},
    snippet::SnippetGenerator,
};

use crate::open_collection;

/// Hit keys allowed in `--fields`, in canonical output order.
const HIT_FIELDS: [&str; 6] = [
    "score",
    "path",
    "title",
    "metadata",
    "snippet",
    "explanation",
];

pub struct SearchOptions {
    pub query: String,
    pub filters: Vec<String>,
    pub fuzzy: u8,
    pub fuzzy_prefix: bool,
    pub any: bool,
    pub limit: usize,
    pub offset: usize,
    pub facets: Vec<String>,
    pub stats: Vec<String>,
    pub explain: bool,
    /// `field:asc|desc` (or plain `field`, defaulting to asc); the field is
    /// `path` or a frontmatter path such as `date`. `None` keeps BM25 order.
    pub sort: Option<String>,
    /// Subset of hit keys to include; empty means all of them.
    pub fields: Vec<String>,
    /// Set to false to drop the `snippet` key even when `fields` is empty.
    pub snippet: bool,
}

/// Sort specification: `path` addresses the stored document path, any other
/// value is a frontmatter path with `\.`-escaped dots, like filters.
struct SortSpec {
    path_field: bool,
    segments: Vec<String>,
    descending: bool,
}

#[derive(Debug, PartialEq)]
enum SortKey {
    Missing,
    Number(f64),
    Text(String),
    Bool(bool),
    Other,
}

impl SortKey {
    fn rank(&self) -> u8 {
        match self {
            SortKey::Missing => 4,
            SortKey::Number(_) => 0,
            SortKey::Text(_) => 1,
            SortKey::Bool(_) => 2,
            SortKey::Other => 3,
        }
    }

    fn compare_present(&self, other: &SortKey) -> Ordering {
        match (self, other) {
            (SortKey::Number(left), SortKey::Number(right)) => left.total_cmp(right),
            (SortKey::Text(left), SortKey::Text(right)) => left.cmp(right),
            (SortKey::Bool(left), SortKey::Bool(right)) => left.cmp(right),
            _ => Ordering::Equal,
        }
    }

    fn from_value(value: &Value, descending: bool) -> SortKey {
        match value {
            Value::Null => SortKey::Missing,
            Value::Bool(flag) => SortKey::Bool(*flag),
            Value::Number(number) => match number.as_f64() {
                Some(number) => SortKey::Number(number),
                None => SortKey::Other,
            },
            Value::String(text) => SortKey::Text(text.clone()),
            // Arrays sort by their smallest element ascending / largest descending.
            Value::Array(items) if !items.is_empty() => items
                .iter()
                .map(|item| SortKey::from_value(item, descending))
                .filter(|key| key != &SortKey::Missing)
                .reduce(|best, key| {
                    let ordering = key
                        .rank()
                        .cmp(&best.rank())
                        .then_with(|| key.compare_present(&best));
                    let key_wins = if descending {
                        ordering == Ordering::Greater
                    } else {
                        ordering == Ordering::Less
                    };
                    if key_wins { key } else { best }
                })
                .unwrap_or(SortKey::Missing),
            Value::Array(_) => SortKey::Missing,
            Value::Object(_) => SortKey::Other,
        }
    }
}

pub fn search_collection(root: &Path, collection: &str, options: &SearchOptions) -> Result<Value> {
    ensure!(
        !options.query.trim().is_empty(),
        "query must not be empty; use '*' to match all documents"
    );
    ensure!(
        (1..=1000).contains(&options.limit),
        "limit must be between 1 and 1000"
    );
    ensure!(options.offset <= 100_000, "offset must not exceed 100000");
    ensure!(options.fuzzy <= 2, "fuzzy distance must be between 0 and 2");
    ensure!(
        !options.fuzzy_prefix || options.fuzzy > 0,
        "fuzzy prefix requires fuzzy distance 1 or 2"
    );
    for field in &options.fields {
        ensure!(
            HIT_FIELDS.contains(&field.as_str()),
            "unknown field {field:?}; valid fields: {}",
            HIT_FIELDS.join(", ")
        );
    }
    let wants =
        |name: &str| options.fields.is_empty() || options.fields.iter().any(|field| field == name);
    let wants_snippet = wants("snippet") && options.snippet;
    let sort = options.sort.as_deref().map(parse_sort).transpose()?;

    let (index, fields) = open_collection(root, collection)?;
    let mut parser = QueryParser::for_index(&index, vec![fields.body, fields.title, fields.meta]);
    parser.set_field_boost(fields.title, 2.0);
    parser.allow_regexes();
    if !options.any {
        parser.set_conjunction_by_default();
    }
    if options.fuzzy > 0 || options.fuzzy_prefix {
        for field in [fields.body, fields.title, fields.meta] {
            parser.set_field_fuzzy(field, options.fuzzy_prefix, options.fuzzy, true);
        }
    }
    let mut query = parser
        .parse_query(&options.query)
        .context("invalid search query")?;
    if !options.filters.is_empty() {
        let mut filter_parser = QueryParser::for_index(&index, vec![fields.exact]);
        filter_parser.set_conjunction_by_default();
        filter_parser.allow_regexes();
        let mut clauses: Vec<(Occur, Box<dyn Query>)> =
            Vec::with_capacity(options.filters.len() + 1);
        clauses.push((Occur::Must, query));
        for filter in &options.filters {
            ensure!(!filter.trim().is_empty(), "filter must not be empty");
            let filter_query = filter_parser
                .parse_query(filter)
                .with_context(|| format!("invalid filter {filter:?}"))?;
            // A filter constrains matches without contributing to their relevance score.
            clauses.push((
                Occur::Must,
                Box::new(ConstScoreQuery::new(filter_query, 0.0)),
            ));
        }
        query = Box::new(BooleanQuery::new(clauses));
    }

    let mut aggregations = Aggregations::default();
    for path in &options.facets {
        ensure!(
            !path.trim().is_empty(),
            "facet metadata path must not be empty"
        );
        let (path, size) = parse_path_size(path, "facet size")?;
        aggregations.insert(
            format!("facet_{path}"),
            Aggregation {
                agg: AggregationVariants::Terms(TermsAggregation {
                    field: format!("exact.{path}"),
                    size: size.or(Some(10)),
                    ..Default::default()
                }),
                sub_aggregation: Default::default(),
            },
        );
    }
    for path in &options.stats {
        ensure!(
            !path.trim().is_empty(),
            "stats metadata path must not be empty"
        );
        aggregations.insert(
            format!("stats_{path}"),
            Aggregation {
                agg: AggregationVariants::Stats(StatsAggregation::from_field_name(format!(
                    "exact.{path}"
                ))),
                sub_aggregation: Default::default(),
            },
        );
    }
    let aggregation_collector = if aggregations.is_empty() {
        None
    } else {
        Some(AggregationCollector::from_aggs(
            aggregations,
            Default::default(),
        ))
    };
    let reader = index.reader()?;
    let searcher = reader.searcher();
    // Sorted search fetches every matching stored document to read its sort key,
    // then paginates in memory; unsorted search keeps Tantivy's TopDocs ranking.
    let (total, top_docs, aggregation_results) = match &sort {
        None => {
            let ranked = TopDocs::with_limit(options.limit)
                .and_offset(options.offset)
                .order_by_score();
            let (total, top_docs, aggregation_results) =
                searcher.search(query.as_ref(), &(Count, ranked, aggregation_collector))?;
            (
                total,
                top_docs
                    .into_iter()
                    .map(|(score, address)| (Some(score), address))
                    .collect::<Vec<_>>(),
                aggregation_results,
            )
        }
        Some(sort) => {
            let (total, addresses, aggregation_results) = searcher.search(
                query.as_ref(),
                &(Count, AllAddresses, aggregation_collector),
            )?;
            let top_docs = sorted_page(
                &searcher,
                &fields,
                sort,
                addresses,
                options.offset,
                options.limit,
            )?;
            (total, top_docs, aggregation_results)
        }
    };
    let snippet_generator = if wants_snippet {
        Some(SnippetGenerator::create(
            &searcher,
            query.as_ref(),
            fields.body,
        )?)
    } else {
        None
    };
    let mut hits = Vec::with_capacity(top_docs.len());
    for (score, address) in top_docs {
        let document: TantivyDocument = searcher.doc(address)?;
        let path = document
            .get_first(fields.path)
            .and_then(|value| value.as_str())
            .context("indexed document is missing its path")?;
        let mut hit = Map::new();
        if wants("score") {
            // Sorted pages have no collector score; explain recovers the same BM25 value.
            let score = match score {
                Some(score) => score,
                None => query.explain(&searcher, address)?.value(),
            };
            hit.insert("score".into(), json!(score));
        }
        if wants("path") {
            hit.insert("path".into(), json!(path));
        }
        if wants("title") {
            let title = document
                .get_first(fields.title)
                .and_then(|value| value.as_str())
                .context("indexed document is missing its title")?;
            hit.insert("title".into(), json!(title));
        }
        if wants("metadata") {
            let metadata = document
                .get_first(fields.meta)
                .context("indexed document is missing its metadata")?;
            hit.insert(
                "metadata".into(),
                serde_json::to_value(OwnedValue::from(metadata))?,
            );
        }
        if wants_snippet {
            let snippet = snippet_generator
                .as_ref()
                .expect("created above when snippets are wanted")
                .snippet_from_doc(&document)
                .to_html();
            hit.insert("snippet".into(), json!(snippet));
        }
        if options.explain && wants("explanation") {
            hit.insert(
                "explanation".into(),
                serde_json::to_value(query.explain(&searcher, address)?)?,
            );
        }
        hits.push(Value::Object(hit));
    }
    let mut result = json!({ "total": total, "hits": hits });
    if let Some(mut aggregation_results) = aggregation_results {
        let mut facets = Map::new();
        let mut stats = Map::new();
        for path in &options.facets {
            let (path, _) = parse_path_size(path, "facet size")?;
            if let Some(value) = aggregation_results.0.remove(&format!("facet_{path}")) {
                facets.insert(path, serde_json::to_value(value)?);
            }
        }
        for path in &options.stats {
            if let Some(value) = aggregation_results.0.remove(&format!("stats_{path}")) {
                stats.insert(path.clone(), serde_json::to_value(value)?);
            }
        }
        result["aggregations"] = json!({ "facets": facets, "stats": stats });
    }
    Ok(result)
}

fn parse_sort(spec: &str) -> Result<SortSpec> {
    let (field, descending) = match spec.rsplit_once(':') {
        Some((field, order)) => {
            let descending = match order {
                "asc" => false,
                "desc" => true,
                _ => bail!("sort order must be asc or desc, got {order:?}"),
            };
            (field, descending)
        }
        None => (spec, false),
    };
    ensure!(!field.trim().is_empty(), "sort field must not be empty");
    if field == "path" {
        Ok(SortSpec {
            path_field: true,
            segments: vec![],
            descending,
        })
    } else {
        Ok(SortSpec {
            path_field: false,
            segments: split_escaped_path(field),
            descending,
        })
    }
}

/// Splits a frontmatter path on dots; `\.` keeps a literal dot, matching filter syntax.
fn split_escaped_path(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut segment = String::new();
    let mut escaped = false;
    for character in path.chars() {
        if escaped {
            segment.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '.' {
            segments.push(std::mem::take(&mut segment));
        } else {
            segment.push(character);
        }
    }
    segments.push(segment);
    segments
}

/// Splits a trailing `:size` suffix used by `--facet path:size`.
fn parse_path_size(spec: &str, label: &str) -> Result<(String, Option<u32>)> {
    if let Some((path, size)) = spec.rsplit_once(':')
        && !path.is_empty()
        && !size.is_empty()
        && size.bytes().all(|b| b.is_ascii_digit())
    {
        let size: u32 = size
            .parse()
            .with_context(|| format!("invalid {label} {size:?}"))?;
        ensure!(
            (1..=100).contains(&size),
            "{label} must be between 1 and 100"
        );
        return Ok((path.to_owned(), Some(size)));
    }
    Ok((spec.to_owned(), None))
}

fn sorted_page(
    searcher: &tantivy::Searcher,
    fields: &crate::Fields,
    sort: &SortSpec,
    addresses: Vec<DocAddress>,
    offset: usize,
    limit: usize,
) -> Result<Vec<(Option<f32>, DocAddress)>> {
    let mut keyed: Vec<(SortKey, String, DocAddress)> = Vec::with_capacity(addresses.len());
    for address in addresses {
        let document: TantivyDocument = searcher.doc(address)?;
        let path = document
            .get_first(fields.path)
            .and_then(|value| value.as_str())
            .context("indexed document is missing its path")?
            .to_owned();
        let key = if sort.path_field {
            SortKey::Text(path.clone())
        } else {
            let meta = match document.get_first(fields.meta) {
                Some(value) => serde_json::to_value(OwnedValue::from(value))?,
                None => Value::Null,
            };
            sort_key_at(&meta, &sort.segments, sort.descending)
        };
        keyed.push((key, path, address));
    }
    keyed.sort_by(|left, right| {
        let ordering = match (&left.0, &right.0) {
            // Documents missing the sort value always go last.
            (SortKey::Missing, SortKey::Missing) => Ordering::Equal,
            (SortKey::Missing, _) => Ordering::Greater,
            (_, SortKey::Missing) => Ordering::Less,
            (left_key, right_key) => {
                let mut ordering = left_key
                    .rank()
                    .cmp(&right_key.rank())
                    .then_with(|| left_key.compare_present(right_key));
                if sort.descending {
                    ordering = ordering.reverse();
                }
                ordering
            }
        };
        // The relative path keeps ties deterministic.
        ordering.then_with(|| left.1.cmp(&right.1))
    });
    Ok(keyed
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(_, _, address)| (None, address))
        .collect())
}

fn sort_key_at(value: &Value, segments: &[String], descending: bool) -> SortKey {
    let mut current = value;
    for segment in segments {
        match current.get(segment) {
            Some(next) => current = next,
            None => return SortKey::Missing,
        }
    }
    SortKey::from_value(current, descending)
}

/// Collects every matching `DocAddress` so sorted search can order them itself.
struct AllAddresses;

impl Collector for AllAddresses {
    type Fruit = Vec<DocAddress>;
    type Child = AllAddressesChild;

    fn for_segment(
        &self,
        segment_local_id: u32,
        _segment: &tantivy::SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        Ok(AllAddressesChild {
            segment_local_id,
            docs: Vec::new(),
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(
        &self,
        mut segment_fruits: Vec<Vec<DocAddress>>,
    ) -> tantivy::Result<Self::Fruit> {
        segment_fruits.sort_by_key(|docs| docs.first().map(|doc| doc.segment_ord).unwrap_or(0));
        Ok(segment_fruits.into_iter().flatten().collect())
    }
}

struct AllAddressesChild {
    segment_local_id: u32,
    docs: Vec<DocAddress>,
}

impl SegmentCollector for AllAddressesChild {
    type Fruit = Vec<DocAddress>;

    fn collect(&mut self, doc: tantivy::DocId, _score: f32) {
        self.docs.push(DocAddress::new(self.segment_local_id, doc));
    }

    fn harvest(self) -> Self::Fruit {
        self.docs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sort_specs_with_orders_and_escaped_dots() {
        let sort = parse_sort("date:desc").unwrap();
        assert_eq!(sort.segments, ["date"]);
        assert!(sort.descending);
        assert!(!parse_sort("year").unwrap().path_field);
        let sort = parse_sort("build\\.version:asc").unwrap();
        assert_eq!(sort.segments, ["build.version"]);
        assert!(!sort.descending);
        assert!(parse_sort("path").unwrap().path_field);
        assert!(parse_sort("").is_err());
        assert!(parse_sort("date:sideways").is_err());
    }

    #[test]
    fn parses_optional_facet_sizes() {
        assert_eq!(parse_path_size("tags", "x").unwrap(), ("tags".into(), None));
        assert_eq!(
            parse_path_size("tags:25", "x").unwrap(),
            ("tags".into(), Some(25))
        );
        // A path may itself contain digits and dots; only a pure-numeric suffix splits.
        assert_eq!(
            parse_path_size("v2.log:3", "x").unwrap(),
            ("v2.log".into(), Some(3))
        );
        assert!(parse_path_size("tags:0", "x").is_err());
        assert!(parse_path_size("tags:101", "x").is_err());
    }

    #[test]
    fn sort_keys_order_types_numbers_and_arrays() {
        use serde_json::json;
        assert_eq!(
            SortKey::from_value(&json!(3.5), false),
            SortKey::Number(3.5)
        );
        assert_eq!(SortKey::from_value(&json!(null), false), SortKey::Missing);
        // Arrays use their smallest value ascending, largest descending.
        assert_eq!(
            SortKey::from_value(&json!([5, 1, 9]), false),
            SortKey::Number(1.0)
        );
        assert_eq!(
            SortKey::from_value(&json!([5, 1, 9]), true),
            SortKey::Number(9.0)
        );
        assert_eq!(
            SortKey::from_value(&json!(["b", "a"]), false),
            SortKey::Text("a".into())
        );
    }
}
