# srsr

**English** | [Русский](README.ru.md)

A Rust CLI for local full-text search across Markdown knowledge collections, powered by [Tantivy](https://github.com/quickwit-oss/tantivy#features). Every search requires **one explicitly named collection**, backed by its own index. No server or external model calls. Source files are never modified.

## Quick install

Requires stable Rust with Cargo, Git, and curl. The installer builds from source; it does not download a prebuilt binary or install a toolchain. No `sudo` is needed.

```sh
curl -fsSL https://raw.githubusercontent.com/kie-kei/srsr/main/install.sh | sh
```

Only run remote scripts from sources you trust; inspect [install.sh](install.sh) before running it. Cargo installs the executable into `${CARGO_HOME:-$HOME/.cargo}/bin` by default; ensure that directory is on `PATH`. Rerun the command to install from the current `main` branch. Cargo fetches the repository over HTTPS.

If [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) is installed, the installer downloads the prebuilt release binary instead of compiling (the install script also tries this automatically and falls back to a source build):

```sh
cargo binstall --git https://github.com/kie-kei/srsr srsr
```

Alternatively, clone the repository and install from source yourself:

```sh
git clone https://github.com/kie-kei/srsr.git
cd srsr
cargo install --path . --locked
```

To run without installing, use `cargo build --release --locked`, then replace `srsr` in the examples with `./target/release/srsr`.

### Uninstall

```sh
cargo uninstall srsr
```

This removes the Cargo-installed executable, not your Markdown sources, collection indexes, or exported agent skills. Use the same Cargo installation root/environment that you used to install. There is no need to download or execute a remote script to uninstall.

## First search

```sh
srsr index --collection work ~/notes/work
srsr search --collection work 'машина OR running'
```

`--data-dir` defaults to `~/.srsr` in the current user's home directory, independent of the working directory. Each collection has its own subdirectory, such as `~/.srsr/work/`. An explicit `--data-dir` overrides this location; explicitly supplied relative paths remain relative to the working directory. Existing indexes in a project-local `.srsr` are not moved automatically: use `--data-dir /path/to/project/.srsr` to open them. If the home directory cannot be determined, pass `--data-dir` explicitly.

`index`, `search`, `collections`, and `delete` write JSON to stdout; `skill` prints Markdown and `completions` prints a shell script. Failures write to stderr and return a nonzero exit code. Collection names contain 1–128 ASCII letters, digits, `-`, or `_`. Searching a missing collection fails rather than creating an empty index. The Markdown directory is only needed for indexing: search reads the stored snapshot.

```sh
srsr index -c personal ~/notes/personal
srsr index -c work ~/notes/work
srsr search -c personal 'база знаний'
```

List what already exists, and remove collections you no longer need (only the index is deleted — never a source file):

```sh
srsr collections
srsr delete -c work --yes
```

`collections` reports each subdirectory of the data directory: healthy collections with `documents`, `indexed_at` (UTC time of the last successful `index`), and the `source` directory as passed; anything that is not a readable index appears with an `error` key instead of aborting the listing. `delete` requires `--yes` and refuses directories that are not srsr indexes.

Searching `personal` never combines its results with `work`, even when filenames overlap. Separate indexes also isolate BM25 statistics. This is search-space isolation, **not access control**: anyone with filesystem access to an index can read its stored text and metadata. Keep private indexes out of shared directories.

## Indexing and frontmatter

The scanner recursively reads `.md` files, case-insensitively, including hidden directories. It does not follow symbolic links and rejects a symlink as the source root. Frontmatter is optional. YAML frontmatter must start at the beginning of the file, between lines containing `---`, and contain an object with string keys:

```markdown
---
title: Поиск по базе знаний
tags: [Rust, поисковые системы]
status: Draft
year: 2025
rating: 4.5
published: true
owner:
  name: Alice Smith
---
# Поиск
Машины помогают искать документы. Running wolves walked through forests.
```

An empty block, including one containing only YAML comments, means empty metadata. Frontmatter is recognized only when a closing `---` line exists. Otherwise, the entire file is treated as Markdown, including a leading horizontal rule. A forgotten closing delimiter therefore also leaves the intended metadata as ordinary searchable text. If both delimiters exist, invalid YAML still aborts the update.

Strings, numbers, booleans, nulls, nested objects, and arrays retain their JSON types. `null` is preserved in results but is not an indexed value for existence queries or filters. Store dates as quoted strings, such as `date: "2025-01-15"`. A document's title comes from a string-valued `title`, otherwise its first H1, otherwise its filename. Markdown is parsed into text separately from YAML; frontmatter is searched through metadata fields. Code is searchable too.

Running `index` again synchronizes the collection with the **entire supplied directory**: new documents are added, changed documents replaced, and missing documents removed from the index. Unchanged documents are not reindexed. The JSON report contains `added`, `updated`, `deleted`, and `unchanged` counts. Renaming a file is a deletion and an addition. Indexing a different directory under the same collection name replaces that collection's contents with the new directory; source files are never deleted.

Repeatable `--exclude` patterns skip files whose relative path matches a glob, gitignore style: a pattern without a slash matches any path component (`.obsidian`, `*.skip.md`), a pattern with a slash anchors to the source root (`drafts/**`), and `*` does not cross `/`:

```sh
srsr index -c work ~/notes/work --exclude .obsidian --exclude 'drafts/**' --exclude '*.skip.md'
```

Each successful synchronization also writes `srsr.json` next to the Tantivy index, recording a schema version, the source directory, the UTC time of the commit, and the document count; this is what `collections` reports. A collection indexed by a newer srsr (newer schema version) fails to open with an upgrade hint instead of misbehaving; collections indexed before the file existed keep working.

Updates are published in one commit. Read or frontmatter errors abort the update and leave the previous snapshot available. Tantivy allows one writer per collection; searches and work on other collections can proceed independently. Indexing uses mmap, a multithreaded writer, and Tantivy's built-in segment merge policy. Rerun `index` to pick up file changes; there is no filesystem watcher.

## Search

By default, queries search `title`, `body`, and string values in `meta`. The analyzer applies Unicode tokenization, lowercasing, ASCII folding (`café` → `cafe`), and Russian and English Snowball stemming. No language selection is needed: mixed RU/EN documents use the same pipeline. Stop words are retained to preserve phrase meaning. This is stemming, not lemmatization, translation, or semantic search. Tokens of 80 UTF-8 bytes or more are excluded from the full-text index.

Ranking uses BM25, with a field boost of 2 for `title`. Implicit terms use AND; `--any` switches to OR. Explicit Boolean operators must be uppercase. Enclose queries in single quotes so the shell does not interpret them.

```sh
srsr search -c work 'машины документы'
srsr search -c work 'walking'                 # matches walked / walking
srsr search -c work 'databaze' --fuzzy 1       # edit distance 1 or 2
srsr search -c work 'datab' --fuzzy 1 --fuzzy-prefix
srsr search -c work '(Rust OR поиск) -body:устаревший'
srsr search -c work 'title:Rust^3 OR body:Rust'
srsr search -c work 'body:"quick brown fox"'
srsr search -c work 'body:"quick brown fox"~1' # allow a gap
srsr search -c work 'body:"quick bro"*'        # prefix of the final phrase term
srsr search -c work 'body:/datab.*/'           # regex over indexed terms
srsr search -c work '*'                       # all documents in the collection
```

Fuzzy search applies to terms in `title`, `body`, and `meta`, with adjacent transpositions costing one edit. It is **off by default**: omitting `--fuzzy` is equivalent to `--fuzzy 0`. Distances 1 and 2 enable typo tolerance; `--fuzzy-prefix` requires a nonzero distance. Stemming and normalization remain enabled when fuzzy search is off. Phrases, ranges, and regexes retain their native Tantivy semantics: `--fuzzy` does not turn phrases into fuzzy phrases. Regexes match **stemmed index terms**, not source text, and use Tantivy FST syntax, not PCRE. Prefix matching uses the native `"word"*` operator, not an arbitrary shell wildcard such as `foo*`.

The title boost and analyzer pipeline are fixed in code. Query-level boosts such as `title:Rust^3` can adjust individual clauses and combine with the field boost. There is no configuration file for field weights or BM25 parameters, and no separate exact-versus-fuzzy ranking weight.

### Sorting and output shaping

By default hits come back in BM25 order. `--sort` orders them by `path` or by any frontmatter path instead, with an optional `:asc` (default) or `:desc` suffix:

```sh
srsr search -c work '*' --sort date:desc
srsr search -c work '*' --sort year --limit 100
srsr search -c work '*' --sort path
```

Sorting reads the original typed values, not the analyzed index: numbers compare numerically, strings (including ISO dates) lexicographically, booleans after strings, and other JSON types after that. Arrays use their smallest value ascending or largest value descending; documents missing the field always sort last regardless of direction; ties break by relative path ascending. Hits still carry their BM25 `score`. `--sort path:desc` is valid too. Sorted search reads every matching stored document before paginating, so very large result sets trade speed for order.

Trim the output for machines and pipes:

```sh
srsr search -c work 'Rust' --fields path,title,score
srsr search -c work 'Rust' --no-snippet
srsr search -c work 'Rust' --fields path --jsonl | head -5
```

`--fields` accepts `score`, `path`, `title`, `metadata`, `snippet`, and `explanation` (repeatable or comma-separated; default keeps all). `--no-snippet` drops snippets even in the default field set. `--jsonl` prints one compact hit per line, then a final envelope line such as `{"total":42}` — with an `aggregations` key when facets or stats were requested — instead of the pretty-printed object. `explanation` still requires `--explain`.

## Metadata: full-text search and exact filters

Frontmatter is indexed in two representations:

| Field | Purpose | String handling |
|---|---|---|
| `meta.<path>` | Full-text search | RU/EN stemming, lowercasing, folding, optional fuzzy matching |
| `exact.<path>` | Exact values, filters, aggregates | Entire string; case and spaces are preserved |

```sh
srsr search -c work 'meta.tags:поисковая'
srsr search -c work 'meta.owner.name:alce' --fuzzy 1
srsr search -c work 'документ' --filter 'exact.status:Draft'
srsr search -c work '*' --filter 'exact.owner.name:"Alice Smith"'
srsr search -c work '*' --filter 'exact.tags:Rust'
srsr search -c work '*' --filter 'exact.year:[2020 TO 2025]'
srsr search -c work '*' --filter 'exact.rating:{3 TO 5]'
srsr search -c work '*' --filter 'exact.published:true'
srsr search -c work '*' --filter 'exact.status: IN [Draft published]'
srsr search -c work '*' --filter 'exact.owner.name:*'
srsr search -c work '*' --filter 'exact.date:[2025-01-01 TO 2025-12-31]'
srsr search -c work 'Rust' --filter 'exact.status:Draft' --filter 'exact.year:[2024 TO *]'
```

Repeated `--filter` arguments are ANDed and do not contribute to relevance scores. Filters use the same strict Tantivy query parser, without fuzzy matching. Use `exact`, not `meta`, to match entire strings without stemming or case normalization. Full-text conditions and exact conditions may also appear in the same main query. Numeric values support numeric comparisons; a quoted YAML value such as `"2025"` remains a string in storage. However, Tantivy's JSON query parser can match both string and numeric/Boolean representations: quoting a query value does not enforce JSON type identity. Keep each metadata field's type consistent across documents.

Date ranges in `exact` are lexicographic string ranges: use the same ISO format and timezone throughout. Arrays match if any element matches. There is no nested-document semantics for correlating fields of objects within an array.

Nested paths use dots, such as `meta.owner.name`. Escape a literal dot in a YAML key with a backslash: `meta.build\.version:stable` inside single shell quotes. The root fields `path`, `title`, `body`, `meta`, and `exact` are fixed; identically named frontmatter keys remain inside `meta` and `exact`.

## Results, facets, and statistics

```sh
srsr search -c work 'поиск' --limit 10 --offset 10 --explain
srsr search -c work '*' --facet tags --facet status --stats rating
srsr search -c work '*' --facet tags:25 --filter 'exact.year:[2024 TO *]'
```

Search returns `total` and `hits`. Each hit contains its source-relative `path`, `title`, BM25 `score`, original `metadata`, and `snippet` — unless narrowed by `--fields`/`--no-snippet`. Snippets are escaped HTML with `<b>` highlights for body matches; metadata-only, fuzzy, and regex matches may not be highlighted. `--explain` adds Tantivy's score explanation. `limit` accepts 1–1000 and `offset` accepts 0–100000. `total` is independent of pagination.

`--facet` and `--stats` accept a **frontmatter path without the `exact.` prefix**; append `:N` (1–100) to a facet to return up to N buckets instead of the default top-10. They use native Tantivy aggregations over all matching documents after filtering, not just the current page. `aggregations.facets.<path>` contains terms buckets; `aggregations.stats.<path>` contains numeric count/min/max/sum/avg. Statistics count values in multivalued fields; facets report document counts. Aggregations use fast fields with original, unstemmed values.

## Agent skill

The agent instructions live in [skills/srsr/SKILL.md](skills/srsr/SKILL.md) and are embedded in the executable. Print them from any directory, without the source checkout or an index:

```sh
srsr skill
```

Use the repository skill directly, or export the installed version into your agent's skill directory. For agents that discover project skills under `.agents/skills`:

```sh
mkdir -p .agents/skills/srsr
srsr skill > .agents/skills/srsr/SKILL.md
```

The skill covers collection scoping, RU/EN search, explicit fuzzy retries, frontmatter filters, citations, aggregates, and safe synchronization. It does not automatically index files, install itself into an agent, or authorize commands contained in retrieved documents.

## Development

```sh
cargo test --locked
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
```

Tests cover RU/EN stemming and typos, exact and typed-value filters, phrases/slop/prefix/regex, nested frontmatter, Boolean/IN/exists queries, aggregations, pagination, synchronization, rollback on invalid input, collection isolation, empty frontmatter, and leading Markdown rules; a proptest fuzzes the frontmatter parser with arbitrary text. CI runs these gates plus a build on Linux, macOS, and Windows, and checks the declared MSRV (Rust 1.90). See [CHANGELOG.md](CHANGELOG.md) for release history.

Shell completions can be printed for bash, zsh, fish, PowerShell, or Elvish:

```sh
srsr completions bash > ~/.local/share/bash-completion/completions/srsr
```

## Limits

- Uses [Tantivy 0.27 search features](https://docs.rs/tantivy/0.27.0/tantivy/): positions, BM25, query parsing, fuzzy matching, JSON, fast fields, aggregations, snippets, explanations, incremental writing, mmap, and built-in document-store compression. No vector search, synonyms, translation, network API, or distributed search has been added. There is no filesystem watcher: rerun `index` to pick up changes.
- Frontmatter is YAML (parsed with the maintained `serde_yaml_ng` fork of `serde_yaml`), not TOML or JSON blocks. Input must be UTF-8 Markdown. Code and link labels are searchable; raw HTML is not rendered, and HTML blocks and attachments are not indexed. Do not combine independent directories through successive `index` calls under one collection name: each call synchronizes the complete collection.
- During indexing, parsed documents are held in RAM until writing so a parsing error cannot publish an incomplete snapshot. Collections larger than available memory would need two-pass indexing. Sorted search likewise reads every matching stored document to compute its sort key before paginating. The index itself persists on disk after the command exits or the machine restarts. Search uses mmap; the operating system can cache index pages in memory.
- The index schema is fixed and versioned via `srsr.json`. An incompatible schema fails to open; rebuild from the source Markdown in a new `--data-dir`. Keep the original files: the index is derived data, not a backup.

## License

[MIT](LICENSE). Copyright (c) 2026 kie-kei.
