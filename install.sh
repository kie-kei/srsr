#!/bin/sh
set -eu

repo=https://github.com/kie-kei/srsr

for program in cargo git; do
    if ! command -v "$program" >/dev/null 2>&1; then
        printf 'srsr: %s is required; install stable Rust and Git first.\n' "$program" >&2
        exit 1
    fi
done

install_from_source() {
    cargo install --git "$repo" --branch main --locked srsr
}

# Run inside a function so a truncated download cannot execute a partial install command.
install_srsr() {
    # Prefer a prebuilt release binary when cargo-binstall is available; any
    # failure (no release yet, missing asset, network) falls back to a source build.
    if command -v cargo-binstall >/dev/null 2>&1; then
        checkout=$(mktemp -d)
        trap 'rm -rf "$checkout"' EXIT INT TERM
        if git clone --quiet --depth 1 --branch main "$repo" "$checkout/srsr" \
            && cargo binstall --no-confirm --force --manifest-path "$checkout/srsr/Cargo.toml" srsr; then
            rm -rf "$checkout"
            return 0
        fi
        printf 'srsr: prebuilt binary unavailable; building from source.\n' >&2
    fi
    install_from_source
}

install_srsr
