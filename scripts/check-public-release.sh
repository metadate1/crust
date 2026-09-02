#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/check-public-release.sh [REVISION...]
  scripts/check-public-release.sh --remote REMOTE

Audit the working tree and every commit reachable from the selected revisions. With no revision,
HEAD is audited. --remote audits every non-symbolic remote-tracking branch for that remote.
EOF
}

fail() {
  printf 'public-release audit failed: %s\n' "$1" >&2
  exit 1
}

for command in git perl sha256sum; do
  command -v "$command" >/dev/null 2>&1 || fail "required command is unavailable: $command"
done

repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || fail "not inside a Git repository"
cd "$repo_root"

declare -a revisions
if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
  usage
  exit 0
elif [[ ${1:-} == "--remote" ]]; then
  [[ $# == 2 ]] || fail "--remote requires exactly one remote name"
  remote=$2
  git remote get-url "$remote" >/dev/null 2>&1 || fail "unknown remote: $remote"
  mapfile -t revisions < <(
    git for-each-ref \
      --format='%(objectname) %(symref)' \
      "refs/remotes/$remote" |
      awk '$2 == "" { print $1 }' |
      sort -u
  )
  ((${#revisions[@]} != 0)) || fail "no remote-tracking branches found for $remote"
elif (($# != 0)); then
  revisions=("$@")
else
  revisions=(HEAD)
fi

for revision in "${revisions[@]}"; do
  git cat-file -e "$revision^{commit}" 2>/dev/null || fail "not a commit: $revision"
done

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
commits="$scratch/commits"
history_paths="$scratch/history-paths"
current_paths="$scratch/current-paths"
objects="$scratch/objects"
blob="$scratch/blob"

git rev-list "${revisions[@]}" | sort -u > "$commits"
: > "$history_paths"
while IFS= read -r commit; do
  git ls-tree -r --full-tree --name-only -z "$commit" >> "$history_paths"
done < "$commits"

git ls-files -z > "$current_paths"
git ls-files --others --exclude-standard -z >> "$current_paths"

# Keep this deliberately broader than .gitignore. A forbidden historical path still fails even if
# a later commit deleted it. The four pinned CRUST PNGs are the only binary-media exceptions.
prohibited='(^|/)(local-data|streams|target|dist|pkg|node_modules|artifacts|captures|screenshots|recordings|coverage|browser-profile|playwright-report|test-results)(/|$)|(^|/)(\.cache|\.pytest_cache|\.mypy_cache|\.ruff_cache|__pycache__)(/|$)|(^|/)fuzz/(artifacts|corpus)(/|$)|(^|/)proptest-regressions(/|$)|(^|/)(memory-card\.json|resume\.json)$|(^|/)\.env($|\.)|(^|/)\.DS_Store$|(^|/)id_(rsa|dsa|ecdsa|ed25519)(\.pub)?$|\.(bin|raw|iso|cue|ccd|sub|ecm|chd|cso|img|mdf|mds|nrg|pbp|nsd|nsf|pbak|exe|dll|bios|zip|7z|rar|tar|tar\.gz|tgz|log|tmp|wasm|wat|rmeta|sav|mcr|srm|state|save|har|webm|mp4|mov|wav|mp3|flac|ogg|p12|pfx|jks|keystore|key|pem)$'
media='\.(png|jpe?g|gif|bmp|tiff?|webp|avif|ico|svg|psd|xcf|blend|fbx|obj|gltf|glb|ttf|otf|woff2?)$'

allowed_media_path() {
  case "$1" in
    artwork/source/crust-game-frame-chroma.png | \
      artwork/source/crust-wordmark-chroma.png | \
      web/assets/crust-game-frame.png | \
      web/assets/crust-wordmark.png)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

check_path_stream() {
  local input=$1 path
  shopt -s nocasematch
  while IFS= read -r -d '' path; do
    [[ $path == .env.example || $path == */.env.example ]] && continue
    if [[ $path =~ $prohibited ]]; then
      fail "forbidden path is tracked, unignored, or reachable in history: $path"
    fi
    if [[ $path =~ $media ]] && ! allowed_media_path "$path"; then
      fail "unapproved media or font path is tracked, unignored, or reachable in history: $path"
    fi
  done < "$input"
  shopt -u nocasematch
}

check_path_stream "$history_paths"
check_path_stream "$current_paths"

git rev-list --objects "${revisions[@]}" |
  git cat-file --batch-check='%(objecttype) %(objectsize) %(objectname) %(rest)' |
  awk '$1 == "blob" { print }' > "$objects"

max_blob_bytes=$((5 * 1024 * 1024))
secret_pattern='A[K]IA[0-9A-Z]{16}|A[S]IA[0-9A-Z]{16}|A[I]za[0-9A-Za-z_-]{35}|g[h][pousr]_[0-9A-Za-z]{30,}|g[i]thub_pat_[0-9A-Za-z_]{20,}|s[k]-[A-Za-z0-9_-]{32,}|x[o][apb]-[0-9A-Za-z-]{20,}|-----BEGIN [A-Z ]*PRIVATE K[E]Y-----'

expected_media_hash() {
  case "$1" in
    artwork/source/crust-game-frame-chroma.png)
      printf '%s\n' 7d73adbb99904a6c9358f2cdd412a06ec90d1c8f5b3119332b84d09973e6990d
      ;;
    artwork/source/crust-wordmark-chroma.png)
      printf '%s\n' 7af925cc41a8c1f561567a9c870518867e11074ee82f8e5ce889852d338504ea
      ;;
    web/assets/crust-game-frame.png)
      printf '%s\n' f065de37fe957794b7f477b1e339adeefa5e41851f32dfe39f7971091a594261
      ;;
    web/assets/crust-wordmark.png)
      printf '%s\n' 220068c73614f4cc55dba334defa810dbd312e5831d1d8bc5f3f85220d44ce5c
      ;;
    *)
      return 1
      ;;
  esac
}

blob_count=0
while IFS=' ' read -r object_type object_size object_id object_path; do
  ((blob_count += 1))
  ((object_size <= max_blob_bytes)) ||
    fail "historical blob exceeds 5 MiB: ${object_path:-$object_id} ($object_size bytes)"

  git cat-file blob "$object_id" > "$blob"
  if perl -e 'local $/; my $data = <>; exit(index($data, "\0") >= 0 ? 0 : 1)' "$blob"; then
    allowed_media_path "$object_path" ||
      fail "unapproved binary blob is reachable in history: ${object_path:-$object_id}"
    actual_hash=$(sha256sum "$blob" | awk '{print $1}')
    expected_hash=$(expected_media_hash "$object_path") ||
      fail "binary path has no pinned hash: $object_path"
    [[ $actual_hash == "$expected_hash" ]] ||
      fail "binary-media hash differs from its approved original: $object_path"
  elif LC_ALL=C grep -aEq "$secret_pattern" "$blob"; then
    fail "credential-like content is reachable in historical blob $object_id"
  fi
done < "$objects"

while IFS= read -r -d '' path; do
  [[ -f $path ]] || continue
  size=$(wc -c < "$path")
  ((size <= max_blob_bytes)) || fail "working-tree file exceeds 5 MiB: $path ($size bytes)"
  if perl -e 'local $/; my $data = <>; exit(index($data, "\0") >= 0 ? 0 : 1)' "$path"; then
    allowed_media_path "$path" || fail "unapproved binary working-tree file: $path"
    actual_hash=$(sha256sum "$path" | awk '{print $1}')
    expected_hash=$(expected_media_hash "$path") || fail "binary path has no pinned hash: $path"
    [[ $actual_hash == "$expected_hash" ]] ||
      fail "binary-media hash differs from its approved original: $path"
  elif LC_ALL=C grep -aEq "$secret_pattern" "$path"; then
    fail "credential-like content exists in working-tree file: $path"
  fi
done < "$current_paths"

git diff --check
git diff --cached --check
git fsck --full --strict --no-dangling >/dev/null

printf 'public-release audit passed: %s commits, %s unique blobs, %s revision tips\n' \
  "$(wc -l < "$commits")" "$blob_count" "${#revisions[@]}"
