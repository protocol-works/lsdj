#!/usr/bin/env bash
# Create the next protected release tag from the current, reviewed main commit.
set -euo pipefail

fail() {
  echo "release: $*" >&2
  exit 1
}

usage() {
  cat >&2 <<'EOF'
Usage: just release

Creates the next vYYYY.MM.N tag for the current UTC month.
EOF
  exit 2
}

[[ "$#" -eq 0 ]] || usage

command -v git >/dev/null 2>&1 || fail "git is required"

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || fail \
  "run this command from the LSDJ repository"
cd "$REPO_ROOT"

ORIGIN_URL="$(git config --get remote.origin.url 2>/dev/null)" || fail \
  "the origin remote is not configured"
[[ "$ORIGIN_URL" =~ (^|[:/])protocol-works/lsdj(\.git)?$ ]] || fail \
  "origin must point to protocol-works/lsdj (found $ORIGIN_URL)"

CURRENT_BRANCH="$(git symbolic-ref --quiet --short HEAD 2>/dev/null)" || fail \
  "releases must be created from the main branch, not a detached HEAD"
[[ "$CURRENT_BRANCH" == "main" ]] || fail \
  "releases must be created from main (currently $CURRENT_BRANCH)"

[[ -z "$(git status --porcelain)" ]] || fail \
  "the working tree must be clean before creating a release"

echo "release: refreshing origin/main and release tags"
git fetch --prune --no-recurse-submodules origin \
  '+refs/heads/main:refs/remotes/origin/main' \
  '+refs/tags/v*:refs/tags/v*'

LOCAL_HEAD="$(git rev-parse HEAD)"
REMOTE_HEAD="$(git rev-parse refs/remotes/origin/main)"
[[ "$LOCAL_HEAD" == "$REMOTE_HEAD" ]] || fail \
  "local main must exactly match origin/main before creating a release"

read -r CURRENT_YEAR CURRENT_MONTH < <(date -u '+%Y %m')
[[ "$CURRENT_YEAR" =~ ^[0-9]{4}$ ]] || fail \
  "could not determine the current UTC year"
[[ "$CURRENT_MONTH" =~ ^(0[1-9]|1[0-2])$ ]] || fail \
  "could not determine the current UTC month"

TAG_PREFIX="v${CURRENT_YEAR}.${CURRENT_MONTH}."
LATEST_RELEASE=0
LATEST_TAG="none"

while IFS= read -r TAG; do
  if [[ "$TAG" =~ ^v${CURRENT_YEAR}\.${CURRENT_MONTH}\.([1-9][0-9]*)$ ]]; then
    RELEASE_NUMBER=$((10#${BASH_REMATCH[1]}))
    if (( RELEASE_NUMBER > LATEST_RELEASE )); then
      LATEST_RELEASE=$RELEASE_NUMBER
      LATEST_TAG=$TAG
    fi
  fi
done < <(git tag --list 'v*')

NEXT_RELEASE=$((LATEST_RELEASE + 1))
NEXT_TAG="${TAG_PREFIX}${NEXT_RELEASE}"
git show-ref --verify --quiet "refs/tags/$NEXT_TAG" && fail \
  "$NEXT_TAG already exists"

printf 'release: latest-this-month=%s next=%s commit=%s\n' \
  "$LATEST_TAG" "$NEXT_TAG" "${LOCAL_HEAD:0:12}"
printf 'Create and push %s? This starts the protected macOS release workflow. [y/N] ' \
  "$NEXT_TAG"
read -r CONFIRMATION
case "$CONFIRMATION" in
  y|Y|yes|YES) ;;
  *)
    echo "release: cancelled"
    exit 0
    ;;
esac

git tag --annotate "$NEXT_TAG" --message "Release $NEXT_TAG"
if ! git push origin "refs/tags/$NEXT_TAG"; then
  git tag --delete "$NEXT_TAG" >/dev/null
  fail "push failed; removed the local $NEXT_TAG tag so it is safe to retry"
fi

echo "release: pushed $NEXT_TAG"
echo "release: an Engineering teammate must now approve the macos-release Environment"
echo "release: https://github.com/protocol-works/lsdj/actions/workflows/macos-release.yml"
