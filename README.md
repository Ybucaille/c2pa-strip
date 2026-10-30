<div align="center">

# c2pa-strip

**Remove C2PA / Content Credentials from your images, without touching a single pixel.**<br>
A small, fast CLI for JPEG, PNG, WebP, HEIF/AVIF, MP4/MOV and PDF: it removes the manifest store and the "made with AI" markers, and nothing else. No re-encoding, no quality loss.

[![CI](https://github.com/Ybucaille/c2pa-strip/actions/workflows/ci.yml/badge.svg)](https://github.com/Ybucaille/c2pa-strip/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Ybucaille/c2pa-strip?color=d97757)](https://github.com/Ybucaille/c2pa-strip/releases/latest)
[![Verified with c2patool](https://img.shields.io/badge/verified%20with-c2patool-6f42c1)](#proof-it-works)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Platforms](https://img.shields.io/badge/platforms-linux%20%7C%20macOS%20%7C%20windows-informational)](https://github.com/Ybucaille/c2pa-strip/releases/latest)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[Proof it works](#proof-it-works) · [Install](#install) · [Usage](#usage) · [How it works](#how-it-works) · [Limits](#limits)

</div>

## Proof it works

A PNG signed by the **official [c2patool](https://github.com/contentauth/c2pa-rs/tree/main/cli)**, then cleaned:

```console
$ c2pa-strip check photo.png
C2PA   photo.png  manifest store 100973 bytes, 1 manifest(s)

$ c2pa-strip remove photo.png
fixed  photo.png -> photo.clean.png  (manifest store 100973 bytes, 1 manifest(s); 219942 -> 118969 bytes)

$ c2patool photo.clean.png
Error: No claim found

$ cmp photo.clean.png original-before-signing.png && echo identical
identical
```

The cleaned file is **byte-identical to the image before it was signed**. Not just the same pixels: the exact same file.

This is not a one-off demo. The [`end-to-end` CI job](.github/workflows/ci.yml) runs on every push and does the following for a PNG, a WebP, an MP4 and an AVIF:

1. signs [`tests/e2e/source.png`](tests/e2e/source.png), [`source.webp`](tests/e2e/source.webp), [`tests/fixtures/tiny.mp4`](tests/fixtures/tiny.mp4) and [`tiny.avif`](tests/fixtures/tiny.avif) with c2patool,
2. checks that `c2pa-strip check` detects the manifest,
3. runs `c2pa-strip remove`,
4. checks that c2patool reports `No claim found` on the result,
5. checks that the image data is unchanged and the file matches the unsigned original.

The signed files are kept in [`tests/fixtures/signed/`](tests/fixtures/signed), and the unit tests replay the same checks offline. The JPEG side is tested against Adobe's reference sample from the C2PA [public test files](https://github.com/c2pa-org/public-testfiles), whose manifest spans 5 APP11 segments.

## Install

**Prebuilt binaries** (Linux x86_64/aarch64 static, macOS Intel/Apple Silicon, Windows) are on the [latest release](https://github.com/Ybucaille/c2pa-strip/releases/latest), with `SHA256SUMS`.

```bash
curl -sL https://github.com/Ybucaille/c2pa-strip/releases/latest/download/c2pa-strip-v0.1.0-x86_64-unknown-linux-musl.tar.gz | tar xz
```

**From source** (Rust 1.88+):

```bash
cargo install --git https://github.com/Ybucaille/c2pa-strip
```

## Usage

```bash
c2pa-strip check photo.jpg pictures/       # report; exit 1 if C2PA or AI markers found, 2 on error
c2pa-strip remove photo.jpg                # writes photo.clean.jpg
c2pa-strip remove photo.jpg -o out.jpg     # choose the output
c2pa-strip remove --in-place -r pictures/  # overwrite, recursing into folders
```

- `check` lists each manifest's label and the size of the store. Its exit code makes it usable in scripts and pre-commit hooks.
- Besides C2PA, both commands handle the "made with AI" markers stored outside it: the IPTC digital source type in XMP when it says generative AI (`trainedAlgorithmicMedia`, `compositeWithTrainedAlgorithmicMedia`; other values such as `digitalCapture` are kept) and the PNG text chunks written by AI image generators (`parameters` from AUTOMATIC1111/Forge, `prompt`/`workflow` from ComfyUI, `invokeai_metadata`).
- `remove` re-checks its own output and **refuses to write** a file that still carries C2PA data or AI markers. Writes go through a temporary file, so `--in-place` never leaves a truncated image behind.
- Folder scans skip the `*.clean.*` files from earlier runs.

## How it works

C2PA stores its manifest in a [JUMBF](https://www.iso.org/standard/84635.html) box, in a container defined by the spec for each format. c2pa-strip parses the container, recognises the manifest store by its JUMBF type UUID (`c2pa` + `0011-0010-8000-00AA00389B71`), and drops only that:

| Format | What is removed                                                 | What is kept                               |
|--------|-----------------------------------------------------------------|--------------------------------------------|
| JPEG   | APP11 segments of the C2PA store (multi-segment stores reassembled) | scan data, EXIF, ICC, other APP11 (JPEG XT) |
| PNG    | `caBX` chunk                                                    | IDAT, all other chunks                      |
| WebP   | `C2PA` chunk                                                    | VP8/VP8L/ALPH, EXIF, ICCP                   |
| HEIF/AVIF, MP4/MOV | top-level C2PA `uuid` box; media offsets (`stco`/`co64`, `iloc`, `tfhd`, `tfra`) moved back | `mdat`, `moov`, all other boxes |
| PDF    | embedded C2PA manifest file (`/AFRelationship /C2PA_Manifest`), its `/AF` and `/EmbeddedFiles` entries | pages and every other object (file rewritten, see below) |
| PNG    | AI generator text chunks (`parameters`, `prompt`, `workflow`, ...) | all other text chunks                  |
| all    | `dcterms:provenance` link and generative-AI `DigitalSourceType` in XMP (PNG XMP compressed or not) | the rest of the XMP |

The image is never decoded. That is why the output can be byte-identical and why the tool is fast. In HEIF/AVIF/MP4/MOV, removing a box shifts the media data, so the absolute offsets pointing at it are rewritten; a file whose offsets cannot be patched safely is refused, never written broken.

PDF is the one exception to byte-for-byte output. C2PA adds its manifest to a PDF as an embedded file through an incremental update, so removing it means rewriting the document as a single revision (with [lopdf](https://crates.io/crates/lopdf)): page content and all other objects are kept as they are, but the file bytes and its cross-reference table change, and earlier revisions are dropped. Encrypted PDFs are refused.

### Compared with C2PAremover

This project started as a Rust rewrite of [ngmisl/C2PAremover](https://github.com/ngmisl/C2PAremover) (Go). The differences come from design choices, not from the language:

| | C2PAremover | c2pa-strip |
|---|---|---|
| Removal | decodes and **re-encodes** (JPEG quality 95) | container surgery, **lossless** |
| EXIF / ICC profile | lost with re-encoding | kept |
| Detection | substring/regex search (`c2pa`, `cai`...) | JUMBF type UUID |
| False positives | any text containing "cai" | none by construction |
| PNG | looks in `tEXt`/`iTXt` | `caBX` (where C2PA actually lives) |
| WebP | not supported | supported |
| HEIF/AVIF, MP4/MOV | not supported | supported, offsets patched |
| PDF | not supported | supported (document rewritten) |
| Verified against c2patool | no | in CI, on every push |

## Limits

- **Invisible watermarks** embedded in the pixels (Google SynthID, Adobe TrustMark, ...) are not metadata. No metadata tool removes them, this one included.
- In HEIF/AVIF and MP4/MOV, only the top-level XMP box is cleaned: XMP stored as a HEIF item or under `moov/udta` is left as is.
- Removing Content Credentials removes provenance information. Use it on your own files and respect the platforms' rules.

## Development

```bash
cargo test                       # unit + integration tests, offline
cargo clippy --all-targets -- -D warnings
tests/e2e/run.sh <c2patool> target/release/c2pa-strip <c2patool>/sample out/   # what the CI e2e job runs
```

## License

[MIT](LICENSE). The test image `tests/fixtures/adobe-20220124-CAICA.jpg` comes from [c2pa-org/public-testfiles](https://github.com/c2pa-org/public-testfiles) (CC BY-SA 4.0).
