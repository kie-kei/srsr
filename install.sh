#!/bin/sh
set -eu

for program in cargo git; do
    if ! command -v "$program" >/dev/null 2>&1; then
        printf 'srsr: %s is required; install stable Rust and Git first.\n' "$program" >&2
        exit 1
    fi
done

# Run inside a function so a truncated download cannot execute a partial install command.
install_srsr() {
    cargo install --git https://github.com/kie-kei/srsr --branch main --locked srsr
}

install_srsr
