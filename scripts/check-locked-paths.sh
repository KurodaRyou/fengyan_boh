#!/usr/bin/env bash
# Locked paths (.github/CODEOWNERS) may change only in the spec:/docs: commits at the start of a branch.
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
    dir=0
    if [[ "$pattern" == */ ]]; then
        dir=1
        pattern=${pattern%/}
    fi
    if [[ "$pattern" == /* ]]; then
        path=${pattern#/}
    elif [[ "$pattern" == */* ]]; then
        path=$pattern
    else
        path="**/$pattern"
    fi
    pathspecs+=(":(glob)$path/**")
    if (( ! dir )); then
        pathspecs+=(":(glob)$path")
    fi
done < <(for rev in "$base" "$head"; do git show "$rev:.github/CODEOWNERS" 2>/dev/null || true; done)

if (( ${#pathspecs[@]} == 0 )); then
    printf 'no CODEOWNERS patterns found at %s or %s\n' "$base" "$head" >&2
    exit 1
fi

failed=0
leading=1
baseline=$(git rev-parse --short "$(git merge-base "$base" "$head")")
while read -r commit; do
    subject=$(git log -1 --format=%s "$commit")
    parents=$(git rev-list --parents -n 1 "$commit" | wc -w)
    if (( parents > 3 )); then
        changed='(octopus merge)'
    elif (( parents == 3 )); then
        # Compare with the merge Git would produce: a combined diff (--cc) hides resolutions that take one
        # parent's version, and `-s ours`. Conflicted files keep their markers there, so conflicts show up too.
        status=0
        recreated=$(git merge-tree --write-tree --no-messages "$commit^1" "$commit^2" | sed -n 1p) || status=$?
        if (( status > 1 )) || [[ -z "$recreated" ]]; then
            printf 'git merge-tree failed for %s\n' "$commit" >&2
            exit 2
        fi
        changed=$(git diff --name-only "$recreated" "$commit" -- "${pathspecs[@]}")
    else
        changed=$(git diff-tree --no-commit-id --name-only -r "$commit" -- "${pathspecs[@]}")
    fi

    if (( leading && parents <= 2 )) && [[ "$subject" == spec:* || "$subject" == docs:* ]]; then
        baseline=$(git rev-parse --short "$commit")
        continue
    fi
    leading=0

    if [[ -n "$changed" ]]; then
        printf '%s %s: changes locked paths after the leading spec:/docs: commits\n' \
            "$(git rev-parse --short "$commit")" "$subject"
        printf '%s\n' "$changed" | sed 's/^/  /'
        failed=1
    fi
done < <(git rev-list --reverse --topo-order "$base..$head")

if (( failed )); then
    printf 'Locked path changes must be in spec:/docs: commits at the start of the branch (baseline %s).\n' "$baseline"
    exit 1
fi
printf 'Locked path check passed (baseline %s).\n' "$baseline"
