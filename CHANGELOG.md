# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). While the major version is 0,
new features ship in minor versions and breaking changes may land in a minor
release too; the CLI's locked JSON output contracts only grow by new keys.

## [Unreleased]

## [0.2.0] - 2026-10-25

### Added

- `srsr collections` lists collections in the data directory with document
  counts, last synchronization time, and source directory; foreign
  directories are reported with a per-entry `error` instead of failing.
- Each committed `index` writes `srsr.json` beside the collection index with
  a schema version, source directory, RFC 3339 synchronization time, and
  document count. Opening a collection whose schema version is newer than
  the binary fails with an actionable upgrade message; collections indexed
  before this file existed keep working.
- `srsr delete -c <name> --yes` removes a collection index after an explicit
  confirmation; source files are never touched, and directories that are not
  srsr indexes are refused.
- `srsr completions <shell>` prints bash, zsh, fish, powershell, or elvish
  completion scripts.
- `--sort <field>[:asc|desc]` orders hits by `path` or any frontmatter path
  (`--sort date:desc`) instead of BM25 relevance. Missing values sort last,
  arrays use their smallest (asc) or largest (desc) value, numbers sort
  before strings, and `--sort path` breaks ties deterministically. Hits keep
  their BM25 `score`.
- `--fields score,path,title,metadata,snippet,explanation` selects hit keys;
  `--no-snippet` drops snippets; `--jsonl` streams one compact hit per line
  followed by a final `{"total":...}` envelope line (with `aggregations`
  when requested).
- `--facet <path>:<size>` returns up to 100 buckets instead of the fixed
  top-10.
- `index --exclude <glob>` (repeatable) skips files whose relative path,
  any path component, or ancestor directory matches the pattern, gitignore
  style (`--exclude .obsidian --exclude 'drafts/**' --exclude '*.skip.md'`).
- GitHub Actions CI (fmt, clippy, tests, build on Linux/macOS/Windows plus
  an MSRV check) and a release workflow attaching prebuilt binaries with
  SHA-256 checksums for five native targets, wired to `cargo-binstall`.
- `rust-version = "1.88"` (the MSRV of Tantivy 0.27) is declared, and
  `install.sh` prefers a prebuilt binary via `cargo binstall` when available,
  falling back to a source build.

### Changed

- YAML frontmatter parsing moved from the archived `serde_yaml` crate to the
  maintained `serde_yaml_ng` fork; parsing behavior is unchanged.

## [0.1.0] - 2026-10-24

### Added

- Initial release: `index`, `search`, and `skill` commands with RU/EN
  stemming, fuzzy matching, exact frontmatter filters, facets, statistics,
  atomic synchronization, and collection isolation.

[Unreleased]: https://github.com/kie-kei/srsr/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/kie-kei/srsr/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/kie-kei/srsr/releases/tag/v0.1.0
