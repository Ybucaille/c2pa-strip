#!/usr/bin/env bash
# End-to-end check against the official c2patool: sign a PNG, a WebP, an MP4
# and an AVIF, remove the manifest with c2pa-strip, then make sure c2patool
# finds nothing and the image data is unchanged.
#
# Usage: run.sh C2PATOOL C2PA_STRIP KEYS_DIR OUT_DIR
#   KEYS_DIR holds es256_private.key and es256_certs.pem (c2patool's samples).
set -euo pipefail

c2patool=$1 strip=$2 keys=$3 out=$4
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
cp "$here/manifest.json" "$keys/es256_private.key" "$keys/es256_certs.pem" "$out/"

for src in "$here/source.png" "$here/source.webp" \
           "$here/../fixtures/tiny.mp4" "$here/../fixtures/tiny.avif"; do
  ext=${src##*.} signed="$out/signed.${src##*.}" clean="$out/clean.${src##*.}"
  echo "::group::$ext"

  "$c2patool" "$src" -m "$out/manifest.json" -o "$signed" -f > "$out/signed.$ext.json"
  echo "c2patool signed $signed ($(wc -c < "$signed") bytes)"

  status=0
  "$strip" check "$signed" || status=$?
  [ "$status" -eq 1 ] || { echo "expected check to exit 1, got $status"; exit 1; }

  "$strip" remove "$signed" -o "$clean"

  if "$c2patool" "$clean" > "$out/clean.$ext.txt" 2>&1; then
    echo "c2patool still reads a manifest in $clean:"; cat "$out/clean.$ext.txt"; exit 1
  fi
  echo "c2patool on cleaned file: $(cat "$out/clean.$ext.txt")"

  case $ext in
    png | webp)
      python3 "$here/same_image_data.py" "$src" "$clean"
      if cmp -s "$src" "$clean"; then echo "cleaned file is byte-identical to the unsigned original"; fi
      ;;
    *)
      # Media offsets were moved back: only a byte-identical file proves it.
      cmp "$src" "$clean"
      echo "cleaned file is byte-identical to the unsigned original"
      ;;
  esac
  echo "::endgroup::"
done
