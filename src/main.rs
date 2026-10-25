use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::{Context, Result, ensure};
use clap::{CommandFactory, Parser, Subcommand};
use srsr::{
    delete_collection,
    ingest::index_collection,
    list_collections,
    search::{SearchOptions, search_collection},
};

#[derive(Parser)]
#[command(version, about = "Isolated Markdown knowledge collections with RU/EN full-text search", long_about = None)]
struct Cli {
    /// Directory holding collection indexes (default: ~/.srsr)
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the bundled agent skill as Markdown
    Skill,
    /// List collections in the data directory; output is JSON
    Collections,
    /// Delete a collection's index (source files are never touched)
    Delete {
        #[arg(short, long)]
        collection: String,
        /// Confirm the deletion of the collection index
        #[arg(long)]
        yes: bool,
    },
    /// Print shell completion scripts to stdout
    Completions { shell: clap_complete::Shell },
    /// Synchronize a collection with a Markdown directory; removed files leave the index
    Index {
        #[arg(short, long)]
        collection: String,
        /// Directory scanned recursively; symbolic links are not followed
        source: PathBuf,
        /// Skip files whose relative path matches (repeatable); e.g. '.obsidian' or 'drafts/**'
        #[arg(long = "exclude")]
        excludes: Vec<String>,
    },
    /// Search only the specified collection; output is JSON
    Search {
        #[arg(short, long)]
        collection: String,
        /// Tantivy query: terms, phrases, Boolean, ranges, sets, regex; '*' matches all
        query: String,
        /// Exact/typed query, ANDed with the search (repeatable); e.g. exact.status:draft
        #[arg(long = "filter", allow_hyphen_values = true)]
        filters: Vec<String>,
        /// Levenshtein distance for text terms; transpositions cost one edit
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=2))]
        fuzzy: u8,
        /// Fuzzy terms also match prefixes (requires --fuzzy 1 or 2)
        #[arg(long)]
        fuzzy_prefix: bool,
        /// Use OR instead of AND between implicit query terms
        #[arg(long)]
        any: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Frontmatter path for a terms aggregation over all matches (repeatable); append :N for up to 100 buckets
        #[arg(long = "facet")]
        facets: Vec<String>,
        /// Numeric frontmatter path for count/min/max/sum/avg (repeatable)
        #[arg(long = "stats")]
        stats: Vec<String>,
        /// Order hits by `path` or a frontmatter path instead of relevance; append :asc or :desc
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Hit keys to include, comma-separated (repeatable): score,path,title,metadata,snippet,explanation
        #[arg(long = "fields", value_delimiter = ',')]
        fields: Vec<String>,
        /// Drop the snippet key from hits
        #[arg(long)]
        no_snippet: bool,
        /// Stream one hit per line, then a final {"total":...} envelope line
        #[arg(long)]
        jsonl: bool,
        /// Include Tantivy's score explanation per hit
        #[arg(long)]
        explain: bool,
    },
}

fn run(cli: Cli) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let data_dir = cli.data_dir.or_else(|| {
        std::env::home_dir()
            .filter(|home| home.is_absolute())
            .map(|home| home.join(".srsr"))
    });
    let value = match cli.command {
        Command::Skill => {
            stdout.write_all(include_str!("../skills/srsr/SKILL.md").as_bytes())?;
            return Ok(());
        }
        Command::Collections => serde_json::json!({ "collections": list_collections(
            data_dir
                .as_deref()
                .context("cannot determine home directory; pass --data-dir")?,
        )? }),
        Command::Delete { collection, yes } => {
            ensure!(yes, "pass --yes to delete the collection index");
            delete_collection(
                data_dir
                    .as_deref()
                    .context("cannot determine home directory; pass --data-dir")?,
                &collection,
            )?;
            serde_json::json!({ "deleted": collection })
        }
        Command::Completions { shell } => {
            let mut command = Cli::command();
            clap_complete::generate(shell, &mut command, "srsr", &mut stdout);
            return Ok(());
        }
        Command::Index {
            collection,
            source,
            excludes,
        } => serde_json::to_value(index_collection(
            data_dir
                .as_deref()
                .context("cannot determine home directory; pass --data-dir")?,
            &collection,
            &source,
            &excludes,
        )?)?,
        Command::Search {
            collection,
            query,
            filters,
            fuzzy,
            fuzzy_prefix,
            any,
            limit,
            offset,
            facets,
            stats,
            sort,
            fields,
            no_snippet,
            jsonl,
            explain,
        } => {
            let result = search_collection(
                data_dir
                    .as_deref()
                    .context("cannot determine home directory; pass --data-dir")?,
                &collection,
                &SearchOptions {
                    query,
                    filters,
                    fuzzy,
                    fuzzy_prefix,
                    any,
                    limit,
                    offset,
                    facets,
                    stats,
                    explain,
                    sort,
                    fields,
                    snippet: !no_snippet,
                },
            )?;
            if jsonl {
                write_jsonl(&mut stdout, result)?;
                return Ok(());
            }
            serde_json::to_value(result)?
        }
    };
    serde_json::to_writer_pretty(&mut stdout, &value)?;
    writeln!(stdout)?;
    Ok(())
}

/// Streams each hit as one compact JSON line, then the envelope without hits.
fn write_jsonl(stdout: &mut impl Write, mut result: serde_json::Value) -> Result<()> {
    let hits = result
        .as_object_mut()
        .and_then(|object| object.remove("hits"))
        .context("search result has no hits");
    for hit in hits?.as_array().context("hits must be an array")? {
        writeln!(stdout, "{}", serde_json::to_string(hit)?)?;
    }
    writeln!(stdout, "{}", serde_json::to_string(&result)?)?;
    Ok(())
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}
