#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
export LC_ALL=C

failed=0
while IFS= read -r -d '' file; do
    file=${file#./}
    # Only integration tests directly under a workspace crate are exempt.
    if [[ "$file" =~ ^crates/[^/]+/tests/ ]]; then
        continue
    fi

    check_event_insert=0
    if [[ "$file" == crates/*.rs && "$file" != crates/boh-storage/src/ledger.rs ]]; then
        check_event_insert=1
    fi

    # Scan the full text so changing case or splitting SQL across lines cannot bypass a rule.
    if ! awk -v check_event_insert="$check_event_insert" '
        { source = source tolower($0) "\n" }
        function reject(pattern, message, prefix, line) {
            if (match(source, pattern)) {
                prefix = substr(source, 1, RSTART)
                line = 1 + gsub(/\n/, "\n", prefix)
                printf "%s:%d: %s\n", FILENAME, line, message
                failed = 1
            }
        }
        END {
            if (check_event_insert) {
                reject("(^|[^[:alnum:]_])insert[[:space:]]+into[[:space:]]+store_events([^[:alnum:]_]|$)",
                       "event inserts are allowed only in boh-storage/src/ledger.rs or crate integration tests")
            }
            reject("(^|[^[:alnum:]_])(insert[[:space:]]+or[[:space:]]+replace|replace[[:space:]]+into|do[[:space:]]+update)([^[:alnum:]_]|$)",
                   "replacement and conflict updates are forbidden outside crate integration tests")
            exit failed
        }
    ' "$file"; then
        failed=1
    fi
done < <(find . -type d \( -name .git -o -name target \) -prune -o \
    -type f \( -name '*.rs' -o -name '*.sql' \) -print0)

if (( failed )); then
    exit 1
fi
printf 'CI source scan passed.\n'
