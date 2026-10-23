use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};
use tantivy::{
    TantivyDocument,
    aggregation::{
        AggregationCollector,
        agg_req::{Aggregation, AggregationVariants, Aggregations},
        bucket::TermsAggregation,
        metric::StatsAggregation,
    },
    collector::{Count, TopDocs},
    query::{BooleanQuery, ConstScoreQuery, Occur, Query, QueryParser},
    schema::{OwnedValue, Value as TantivyValue},
    snippet::SnippetGenerator,
};

use crate::open_collection;

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
        aggregations.insert(
            format!("facet_{path}"),
            Aggregation {
                agg: AggregationVariants::Terms(TermsAggregation {
                    field: format!("exact.{path}"),
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
    let top_docs = TopDocs::with_limit(options.limit)
        .and_offset(options.offset)
        .order_by_score();
    let (total, top_docs, aggregation_results) =
        searcher.search(query.as_ref(), &(Count, top_docs, aggregation_collector))?;
    let snippet_generator = SnippetGenerator::create(&searcher, query.as_ref(), fields.body)?;
    let mut hits = Vec::with_capacity(top_docs.len());
    for (score, address) in top_docs {
        let document: TantivyDocument = searcher.doc(address)?;
        let path = document
            .get_first(fields.path)
            .and_then(|value| value.as_str())
            .context("indexed document is missing its path")?;
        let title = document
            .get_first(fields.title)
            .and_then(|value| value.as_str())
            .context("indexed document is missing its title")?;
        let metadata = document
            .get_first(fields.meta)
            .context("indexed document is missing its metadata")?;
        let mut hit = json!({
            "score": score,
            "path": path,
            "title": title,
            "metadata": OwnedValue::from(metadata),
            "snippet": snippet_generator.snippet_from_doc(&document).to_html(),
        });
        if options.explain {
            hit["explanation"] = serde_json::to_value(query.explain(&searcher, address)?)?;
        }
        hits.push(hit);
    }
    let mut result = json!({ "total": total, "hits": hits });
    if let Some(mut aggregation_results) = aggregation_results {
        let mut facets = Map::new();
        let mut stats = Map::new();
        for path in &options.facets {
            if let Some(value) = aggregation_results.0.remove(&format!("facet_{path}")) {
                facets.insert(path.clone(), serde_json::to_value(value)?);
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
