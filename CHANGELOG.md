# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Generative-AI markers outside C2PA: IPTC `DigitalSourceType` in XMP
  (`trainedAlgorithmicMedia`, `compositeWithTrainedAlgorithmicMedia`) and PNG
  text chunks from AI image generators (`parameters`, `prompt`, `workflow`,
  `invokeai_metadata`). `check` reports them, `remove` removes them.
- HEIF, AVIF, MP4 and MOV: the top-level C2PA `uuid` box is removed and the
  media offsets (`stco`/`co64`, `iloc`, `tfhd`, `tfra`) are moved back; the
  top-level XMP box is cleaned. End-to-end CI test extended to MP4 and AVIF.
- Compressed XMP in PNG `iTXt` chunks is cleaned too.
- PDF: the embedded C2PA manifest file is removed along with its `/AF` and
  `/EmbeddedFiles` entries, and the document XMP is cleaned. Unlike the other
  formats the file is rewritten (with `lopdf`), not edited in place.

### Changed

- `check` also exits with 1 when a file carries AI markers.

## [0.1.0] - 2026-10-26

First release.

### Added

- `check` command: reports C2PA manifest stores and their manifest labels,
  exits with 1 when any file carries C2PA content.
- `remove` command: lossless removal, writing `<name>.clean.<ext>`, a chosen
  `--output`, or `--in-place`; directories with `-r` for recursion.
- JPEG: APP11 JUMBF segments recognised as a C2PA manifest store by type UUID,
  multi-segment stores reassembled; other APP11 users (JPEG XT) are kept.
- PNG: `caBX` chunk. WebP: `C2PA` chunk.
- `dcterms:provenance` link removed from XMP in all three formats.
- Output is re-checked before writing; writes are atomic.
- End-to-end CI test against the official `c2patool`.
- Prebuilt binaries for Linux (x86_64, aarch64, static musl), macOS
  (Intel, Apple Silicon) and Windows.

[0.1.0]: https://github.com/Ybucaille/c2pa-strip/releases/tag/v0.1.0
