#!/usr/bin/env bash
set -euo pipefail

# Syntax-check the shell snippets embedded in the project's Markdown docs.
#
# Extracts every ```bash / ```sh fenced code block from the first-party Markdown files
# (all tracked *.md except the vendored `vendor/` tree) and runs `bash -n` on each — a
# syntax check only, nothing is executed. This is wired into `just doc-shell` / `just
# doc-check` and CI so a doc example with broken shell syntax fails the build.
#
# Illustrative, non-executable snippets (console pastes, pseudo-code, sample output) should
# be fenced as ```text (or another non-shell language) so they are skipped here.
#
# Usage:
#   scripts/check_doc_shell.sh [file.md ...]
# With no arguments it checks every first-party Markdown file in the repo.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )"
REPO_ROOT="$( cd "${SCRIPT_DIR}/.." &> /dev/null && pwd )"
cd "${REPO_ROOT}"

# Collect the files to check: explicit args, or every tracked *.md outside vendor/.
files=()
if [ "$#" -gt 0 ]; then
    files=("$@")
else
    while IFS= read -r f; do
        files+=("$f")
    done < <(git ls-files '*.md' | grep -v '^vendor/')
fi

tick='```'
status=0
checked=0

for file in "${files[@]}"; do
    [ -f "$file" ] || { echo "skip (not a file): $file" >&2; continue; }
    in_block=0
    start_line=0
    lineno=0
    block=""
    while IFS= read -r line || [ -n "$line" ]; do
        lineno=$((lineno + 1))
        # Trim a trailing carriage return / spaces so a bare closing fence always matches.
        trimmed="${line%$'\r'}"
        trimmed="${trimmed%"${trimmed##*[![:space:]]}"}"
        if [ "$in_block" -eq 0 ]; then
            case "$trimmed" in
                "${tick}bash"|"${tick}bash "*|"${tick}sh"|"${tick}sh "*)
                    in_block=1
                    start_line=$lineno
                    block=""
                    ;;
            esac
        else
            if [ "$trimmed" = "$tick" ]; then
                in_block=0
                checked=$((checked + 1))
                if ! errmsg="$(printf '%s' "$block" | bash -n 2>&1)"; then
                    status=1
                    echo "✗ ${file}:${start_line} — shell block failed 'bash -n':" >&2
                    printf '%s\n' "$errmsg" | sed 's/^/    /' >&2
                fi
            else
                block+="${line}"$'\n'
            fi
        fi
    done < "$file"
    if [ "$in_block" -ne 0 ]; then
        echo "✗ ${file}:${start_line} — unterminated shell code fence" >&2
        status=1
    fi
done

if [ "$status" -eq 0 ]; then
    echo "doc-shell: ${checked} shell code block(s) OK ('bash -n')."
else
    echo "doc-shell: shell syntax errors found (see above)." >&2
fi
exit "$status"
