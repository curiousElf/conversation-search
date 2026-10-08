#!/bin/bash
# Snapshot both transcript roots and prove, at every phase exit, that nothing
# recorded in the snapshot was lost or rewritten.
#
# Read-only: this script only reads ~/.claude/projects and ~/.codex/sessions.
#
#   scripts/snapshot_transcripts.sh          # take the snapshot (once)
#   scripts/snapshot_transcripts.sh verify   # re-check the sample
#
# Transcripts of live sessions grow while the CLIs run, so a whole-file hash
# would differ for an innocent reason. The check that actually matters is
# append-only-ness: for every sampled file, the bytes recorded in the snapshot
# must still be an exact prefix of the file on disk. A file that shrank, lost
# bytes, or had history rewritten fails; one that simply grew passes.
set -euo pipefail

CLAUDE_ROOT="$HOME/.claude/projects"
CODEX_ROOT="$HOME/.codex/sessions"
BACKUP_DIR="$HOME/Backups"
MANIFEST="$BACKUP_DIR/transcripts-manifest.txt"
SAMPLE_LIST="$BACKUP_DIR/transcripts-sample.txt"

archive_path() {
  ls -1t "$BACKUP_DIR"/transcripts-*.tar 2>/dev/null | head -1
}

hash_bytes() {
  # sha256 of the first $2 bytes of file $1
  head -c "$2" "$1" | shasum -a 256 | awk '{print $1}'
}

case "${1:-snapshot}" in
  snapshot)
    mkdir -p "$BACKUP_DIR"
    archive="$BACKUP_DIR/transcripts-$(date +%Y-%m-%d).tar"
    if [ ! -f "$archive" ]; then
      echo "Archiving both transcript roots to $archive"
      tar -cf "$archive" -C "$HOME" .claude/projects .codex/sessions
    else
      echo "Reusing existing archive $archive"
    fi
    ls -lh "$archive"

    echo "Selecting a fixed 50-file sample"
    { find "$CLAUDE_ROOT" -name '*.jsonl' -type f | sort | awk 'NR % 45 == 1' | head -40
      find "$CODEX_ROOT" -name '*.jsonl' -type f | sort | awk 'NR % 15 == 1' | head -10
    } > "$SAMPLE_LIST"
    echo "sample size: $(wc -l < "$SAMPLE_LIST" | tr -d ' ')"

    echo "Recording size and hash for each sampled file, from the archive"
    workdir="$(mktemp -d)"
    : > "$MANIFEST"
    while IFS= read -r file; do
      member="${file#"$HOME"/}"
      if tar -xf "$archive" -C "$workdir" "$member" 2>/dev/null; then
        size="$(stat -f%z "$workdir/$member")"
        printf '%s %s %s\n' "$size" "$(hash_bytes "$workdir/$member" "$size")" "$file" >> "$MANIFEST"
      else
        echo "warning: $member is not in the archive, skipping" >&2
      fi
    done < "$SAMPLE_LIST"
    rm -rf "$workdir"
    echo "Manifest written to $MANIFEST ($(wc -l < "$MANIFEST" | tr -d ' ') files)"
    ;;

  verify)
    if [ ! -f "$MANIFEST" ]; then
      echo "No manifest at $MANIFEST — run the snapshot first" >&2
      exit 1
    fi
    failures=0
    checked=0
    grew=0
    while read -r size want file; do
      checked=$((checked + 1))
      if [ ! -f "$file" ]; then
        echo "LOST      $file" >&2
        failures=$((failures + 1))
        continue
      fi
      now="$(stat -f%z "$file")"
      if [ "$now" -lt "$size" ]; then
        echo "SHRANK    $file ($size -> $now bytes)" >&2
        failures=$((failures + 1))
        continue
      fi
      if [ "$(hash_bytes "$file" "$size")" != "$want" ]; then
        echo "REWRITTEN $file (first $size bytes changed)" >&2
        failures=$((failures + 1))
        continue
      fi
      [ "$now" -gt "$size" ] && grew=$((grew + 1))
    done < "$MANIFEST"

    if [ "$failures" -eq 0 ]; then
      echo "MANIFEST OK: $checked files intact, $grew of them grew (appended to) since the snapshot"
    else
      echo "MANIFEST FAILED: $failures of $checked files lost, shrank, or were rewritten" >&2
      exit 1
    fi
    ;;

  *)
    echo "usage: $0 [snapshot|verify]" >&2
    exit 2
    ;;
esac
