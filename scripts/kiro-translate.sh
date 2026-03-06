#!/bin/bash
# kiro-translate.sh — Translate C test cases to Rust using kiro-cli
#
# Usage:
#   ./kiro-translate.sh <test-corpus-dir> <output-dir> [--filter regex]
#
# Output is compatible with the Test-Corpus Rust runner:
#   python3 -m runtests.rust --root <output-dir> --subset <output-dir> --keep-going

set -euo pipefail

INPUT_DIR="${1:?Usage: $0 <test-corpus-dir> <output-dir> [--filter regex]}"
OUTPUT_DIR="${2:?Usage: $0 <test-corpus-dir> <output-dir> [--filter regex]}"
FILTER="${3:-}"

if [[ "$FILTER" == "--filter" ]]; then
    FILTER="${4:?--filter requires a regex argument}"
fi

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
LOG_DIR="$OUTPUT_DIR/logs_$TIMESTAMP"
mkdir -p "$LOG_DIR"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

total=0
translated=0
failed=0

for test_case in "$INPUT_DIR"/*/; do
    name=$(basename "$test_case")

    # Must have test_case/ and test_vectors/
    [[ -d "$test_case/test_case" && -d "$test_case/test_vectors" ]] || continue

    # Apply filter if provided
    if [[ -n "$FILTER" ]] && ! echo "$name" | grep -qE "$FILTER"; then
        continue
    fi

    total=$((total + 1))
    echo "[$total] Translating: $name"

    # Set up output directory
    out="$OUTPUT_DIR/$name"
    mkdir -p "$out/translated_rust"

    # Copy test_vectors and runner (for _lib cases), cleaning first to avoid nesting
    rm -rf "$out/test_vectors" "$out/runner"
    cp -r "$test_case/test_vectors" "$out/"
    [[ -d "$test_case/runner" ]] && cp -r "$test_case/runner" "$out/"

    # Load prompt based on project type
    if [[ "$name" == *_lib ]]; then
        prompt=$(cat "$SCRIPT_DIR/prompts/library.md")
    else
        prompt=$(cat "$SCRIPT_DIR/prompts/executable.md")
    fi

    # Invoke kiro-cli, capturing failures without killing the script
    if (
        cd "$out/translated_rust"
        mkdir -p c_src
        cp -a "$test_case/test_case/." c_src/

        kiro-cli chat \
            --no-interactive \
            --trust-all-tools \
            "$prompt" \
            2>&1 | tee "$LOG_DIR/$name.log" | tail -5
    ); then
        if [[ -f "$out/translated_rust/Cargo.toml" ]]; then
            translated=$((translated + 1))
            echo "  ✅ $name translated"
        else
            failed=$((failed + 1))
            echo "  ❌ $name failed (no Cargo.toml produced)"
        fi
    else
        failed=$((failed + 1))
        echo "  ❌ $name failed (kiro-cli error)"
    fi
    echo
done

echo "========================================"
echo "Done: $translated/$total translated, $failed failed"
echo "Logs: $LOG_DIR"
echo ""
echo "To validate, run from the Test-Corpus repo:"
echo "  python3 -m runtests.rust --root $OUTPUT_DIR --subset $OUTPUT_DIR --keep-going"
