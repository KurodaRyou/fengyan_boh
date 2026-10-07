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

empty_blob=$(git hash-object -t blob /dev/null)
conflict_index=$(mktemp)
trap 'rm -f "$conflict_index"' EXIT
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
        # parent's version, and `-s ours`.
        status=0
        merged=$(git merge-tree --write-tree --name-only --no-messages "$commit^1" "$commit^2") || status=$?
        recreated=$(printf '%s\n' "$merged" | sed -n 1p)
        if (( status > 1 )) || [[ -z "$recreated" ]]; then
            printf 'git merge-tree failed for %s\n' "$commit" >&2
            exit 2
        fi
        changed=$(git diff --name-only "$recreated" "$commit" -- "${pathspecs[@]}")
        # Any conflict on a locked path fails: some (e.g. modify/delete) leave no markers in the re-created tree.
        conflicted=$(printf '%s\n' "$merged" | sed 1d)
        if [[ -n "$conflicted" ]]; then
            # Match the reported paths themselves: some (e.g. the source of a rename/rename) exist in no tree here.
            rm -f "$conflict_index"
            printf '%s\n' "$conflicted" | sed "s/^/100644 $empty_blob\t/" |
                GIT_INDEX_FILE=$conflict_index git update-index --index-info
            changed+=$'\n'$(GIT_INDEX_FILE=$conflict_index git ls-files -- "${pathspecs[@]}")
        fi
        changed=$(printf '%s\n' "$changed" | sed '/^$/d' | sort -u)
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
