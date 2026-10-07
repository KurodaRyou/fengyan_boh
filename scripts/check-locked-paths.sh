#!/usr/bin/env bash
# Locked paths (.github/CODEOWNERS) may change only in the spec:/docs: commits at the start of a branch, and the
# branch's own commits (base..head) must be linear.
# Usage: scripts/check-locked-paths.sh [<base> [<head>]]   (defaults: origin/main HEAD)
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
export LC_ALL=C

base=${1:-origin/main}
head=${2:-HEAD}

# Union of the base and head versions: dropping a CODEOWNERS line on the branch must not unlock that path.
pathspecs=()
while read -r pattern _; do
    [[ -z "$pattern" || "$pattern" == \#* ]] && continue
    pattern=${pattern%/}
    if [[ "$pattern" == /* ]]; then
        path=${pattern#/}
    elif [[ "$pattern" == */* ]]; then
        path=$pattern
    else
        path="**/$pattern"
    fi
    # Match the path itself too: replacing a locked directory with a symlink must not slip through.
    pathspecs+=(":(glob)$path" ":(glob)$path/**")
done < <(for rev in "$base" "$head"; do git show "$rev:.github/CODEOWNERS" 2>/dev/null || true; done)

if (( ${#pathspecs[@]} == 0 )); then
    printf 'no CODEOWNERS patterns found at %s or %s\n' "$base" "$head" >&2
    exit 1
fi

failed=0
leading=1
baseline=$(git rev-parse --short "$(git merge-base "$base" "$head")")
while read -r commit; do
    short=$(git rev-parse --short "$commit")
    subject=$(git log -1 --format=%s "$commit")
    if (( $(git rev-list --parents -n 1 "$commit" | wc -w) > 2 )); then
        printf '%s %s: merge commit; bring slice branches up to date with main by rebase\n' "$short" "$subject"
        failed=1
        leading=0
        continue
    fi

    if (( leading )) && [[ "$subject" == spec:* || "$subject" == docs:* ]]; then
        baseline=$short
        continue
    fi
    leading=0

    changed=$(git diff-tree --no-commit-id --name-only -r "$commit" -- "${pathspecs[@]}")
    if [[ -n "$changed" ]]; then
        printf '%s %s: changes locked paths after the leading spec:/docs: commits\n' "$short" "$subject"
        printf '%s\n' "$changed" | sed 's/^/  /'
        failed=1
    fi
done < <(git rev-list --reverse --topo-order "$base..$head")

if (( failed )); then
    printf 'Slice commits must be linear, and locked path changes must be in the spec:/docs: commits at the start\n'
    printf 'of the branch (baseline %s).\n' "$baseline"
    exit 1
fi
printf 'Locked path check passed (baseline %s).\n' "$baseline"
