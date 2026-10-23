use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::Result;
use clap::{Parser, Subcommand};
use srsr::{
    ingest::index_collection,
    search::{SearchOptions, search_collection},
};

#[derive(Parser)]
#[command(version, about = "Isolated Markdown knowledge collections with RU/EN full-text search", long_about = None)]
struct Cli {
    /// Directory holding independent collection indexes (not the Markdown source)
    #[arg(long, global = true, default_value = ".srsr")]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the bundled agent skill as Markdown
    Skill,
    /// Synchronize a collection with a Markdown directory; removed files leave the index
    Index {
        #[arg(short, long)]
        collection: String,
        /// Directory scanned recursively; symbolic links are not followed
        source: PathBuf,
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
        /// Frontmatter path for a terms aggregation over all matches (repeatable)
        #[arg(long = "facet")]
        facets: Vec<String>,
        /// Numeric frontmatter path for count/min/max/sum/avg (repeatable)
        #[arg(long = "stats")]
        stats: Vec<String>,
        /// Include Tantivy's score explanation per hit
        #[arg(long)]
        explain: bool,
    },
}

fn run(cli: Cli) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let value = match cli.command {
        Command::Skill => {
            stdout.write_all(include_str!("../skills/srsr/SKILL.md").as_bytes())?;
            return Ok(());
        }
        Command::Index { collection, source } => {
            serde_json::to_value(index_collection(&cli.data_dir, &collection, &source)?)?
        }
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
            explain,
        } => search_collection(
            &cli.data_dir,
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
            },
        )?,
    };
    serde_json::to_writer_pretty(&mut stdout, &value)?;
    writeln!(stdout)?;
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
