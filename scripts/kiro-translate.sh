#!/bin/bash
# kiro-translate.sh — Translate C test cases to Rust using kiro-cli
#
# Usage:
#   ./kiro-translate.sh <test-corpus-dir> <output-dir> [--filter regex]
#
# The output is compatible with the Test-Corpus Rust runner:
#   python3 -m runtests.rust --root <test-corpus-dir> --subset <output-dir>
#
# Each test case gets a translated_rust/ directory alongside its test_vectors/.

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

    # Set up output directory mirroring Test-Corpus structure
    out="$OUTPUT_DIR/$name"
    mkdir -p "$out/translated_rust"

    # Copy test_vectors and runner (for _lib cases) so the Rust runner can find them
    cp -r "$test_case/test_vectors" "$out/"
    [[ -d "$test_case/runner" ]] && cp -r "$test_case/runner" "$out/"

    # Load prompt based on project type
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    if [[ "$name" == *_lib ]]; then
        prompt=$(cat "$SCRIPT_DIR/prompts/library.md")
    else
        prompt=$(cat "$SCRIPT_DIR/prompts/executable.md")
    fi

    # Invoke kiro-cli
    (
        cd "$out/translated_rust"
        mkdir -p c_src
        cp -r "$test_case/test_case/src/"* c_src/ 2>/dev/null || true
        cp "$test_case/test_case/include/"* c_src/ 2>/dev/null || true

        kiro-cli chat \
            --no-interactive \
            --trust-all-tools \
            "$prompt" \
            2>&1 | tee "$LOG_DIR/$name.log" | tail -5
    )

    if [[ -f "$out/translated_rust/Cargo.toml" ]]; then
        translated=$((translated + 1))
        echo "  ✅ $name translated"
    else
        failed=$((failed + 1))
        echo "  ❌ $name failed"
    fi
    echo
done

echo "========================================"
echo "Done: $translated/$total translated, $failed failed"
echo "Logs: $LOG_DIR"
echo ""
echo "To validate, run from the Test-Corpus repo:"
echo "  python3 -m runtests.rust --root $OUTPUT_DIR --keep-going"
