#!/usr/bin/env bash
# Fork release tags record where on upstream the fork is up to date:
#
#   vX.Y.P+fork.N.gSHA
#
#   X.Y.Z  newest upstream stable release tag (vX.Y.Z) the fork commit contains
#   P      Z * 1000 + fork build (1..999), so jcode's numeric comparison orders
#          fork releases within one upstream release and across releases
#   N      upstream commits merged past vX.Y.Z (0 = exactly that release)
#   SHA    abbreviated upstream commit the fork is merged up to
#
# Example: v1.0.2001+fork.85.gbe8c616 is fork build 1 on upstream v1.0.2 plus
# 85 commits, up to be8c616. It reads like `git describe` (v1.0.2-85-gbe8c616).
#
# Usage:
#   scripts/fork/release-tag.sh next [REF]          print the next tag for REF (default HEAD)
#   scripts/fork/release-tag.sh verify TAG [REF]    check TAG against REF; print key=value facts
#
# Environment: UPSTREAM_URL (default https://github.com/1jehuang/jcode),
# UPSTREAM_BRANCH (default master), FORK_REMOTE (default origin, used by next).
# Upstream commits are fetched with --no-tags into FETCH_HEAD only; upstream
# tags are read with ls-remote and never imported into the fork.
set -euo pipefail

UPSTREAM_URL=${UPSTREAM_URL:-https://github.com/1jehuang/jcode}
UPSTREAM_BRANCH=${UPSTREAM_BRANCH:-master}
FORK_REMOTE=${FORK_REMOTE:-origin}
TAG_RE='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.([1-9][0-9]{3,})\+fork\.(0|[1-9][0-9]*)\.g([0-9a-f]{7,40})$'

die() {
  echo "release-tag: $*" >&2
  exit 1
}

# Sets: base_sha, upstream_tag, upstream_tag_sha, distance, X, Y, Z
describe_upstream_base() {
  local ref=$1 commit upstream_head
  commit=$(git rev-parse --verify "$ref^{commit}") || die "unknown ref: $ref"
  git fetch -q --no-tags "$UPSTREAM_URL" "refs/heads/$UPSTREAM_BRANCH" ||
    die "cannot fetch $UPSTREAM_URL $UPSTREAM_BRANCH"
  upstream_head=$(git rev-parse FETCH_HEAD)
  base_sha=$(git merge-base "$commit" "$upstream_head") ||
    die "$ref shares no history with upstream $UPSTREAM_BRANCH"

  # Stable release tags only (vX.Y.Z), peeled to commits, newest version first.
  local tags
  tags=$(git ls-remote --tags "$UPSTREAM_URL" 'v*' | awk '
    { name = $2; sub("^refs/tags/", "", name); peeled = sub("\\^\\{\\}$", "", name) }
    name ~ /^v[0-9]+\.[0-9]+\.[0-9]+$/ { if (peeled || !(name in sha)) sha[name] = $1 }
    END { for (n in sha) { split(substr(n, 2), v, "."); print v[1], v[2], v[3], n, sha[n] } }' |
    sort -k1,1nr -k2,2nr -k3,3nr | cut -d' ' -f4-)
  [ -n "$tags" ] || die "upstream has no vX.Y.Z release tags"

  upstream_tag=
  local name sha
  while read -r name sha; do
    if git merge-base --is-ancestor "$sha" "$base_sha" 2>/dev/null; then
      upstream_tag=$name
      upstream_tag_sha=$sha
      break
    fi
  done <<<"$tags"
  [ -n "$upstream_tag" ] || die "$ref contains no upstream release tag"

  distance=$(git rev-list --count "$upstream_tag_sha..$base_sha")
  IFS=. read -r X Y Z <<<"${upstream_tag#v}"
}

cmd_next() {
  local ref=${1:-HEAD}
  describe_upstream_base "$ref"
  local lo=$((Z * 1000)) hi=$((Z * 1000 + 999)) max=0 patch
  # Existing fork tags for the same upstream release decide the build number.
  while read -r patch; do
    if [ "$patch" -gt "$lo" ] && [ "$patch" -le "$hi" ] && [ "$patch" -gt "$max" ]; then
      max=$patch
    fi
  done < <(git ls-remote --tags "$FORK_REMOTE" "v$X.$Y.*" |
    sed -nE "s#^[0-9a-f]+[[:space:]]+refs/tags/v$X\.$Y\.([0-9]+)\+fork\..*#\1#p" |
    sed 's/\^{}$//')
  local build=1
  [ "$max" -gt 0 ] && build=$((max - lo + 1))
  [ "$build" -le 999 ] || die "fork build for $upstream_tag would exceed 999"
  echo "v$X.$Y.$((lo + build))+fork.$distance.g${base_sha:0:7}"
}

cmd_verify() {
  local tag=${1:?verify needs a TAG} ref=${2:-HEAD}
  [[ $tag =~ $TAG_RE ]] || die "expected vX.Y.P+fork.N.gSHA (P = upstream patch * 1000 + build), got: $tag"
  local tx=${BASH_REMATCH[1]} ty=${BASH_REMATCH[2]} tp=${BASH_REMATCH[3]}
  local tn=${BASH_REMATCH[4]} tsha=${BASH_REMATCH[5]}
  describe_upstream_base "$ref"
  local tz=$((tp / 1000)) build=$((tp % 1000))
  [ "$build" -ge 1 ] || die "$tag: fork build (patch % 1000) must be 1..999"
  [ "v$tx.$ty.$tz" = "$upstream_tag" ] ||
    die "$tag claims upstream v$tx.$ty.$tz, but the newest upstream release in $ref is $upstream_tag"
  [ "$tn" = "$distance" ] ||
    die "$tag claims $tn commits past $upstream_tag, but $ref has $distance"
  [[ $base_sha == "$tsha"* ]] ||
    die "$tag claims upstream g$tsha, but $ref is merged up to $base_sha"
  printf 'upstream_tag=%s\nupstream_distance=%s\nupstream_sha=%s\nfork_build=%s\n' \
    "$upstream_tag" "$distance" "$base_sha" "$build"
}

case ${1:-} in
  next) shift; cmd_next "$@" ;;
  verify) shift; cmd_verify "$@" ;;
  *) sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2 ;;
esac
