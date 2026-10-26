# c2pa-strip

Detects and removes C2PA (Content Credentials) manifests from JPEG, PNG and
WebP images, **without re-encoding**: image data, EXIF, ICC profiles and the
rest of the XMP are kept byte for byte.

A Rust rewrite of the idea behind [ngmisl/C2PAremover](https://github.com/ngmisl/C2PAremover).

## What is removed

| Format | Container                                                   |
|--------|-------------------------------------------------------------|
| JPEG   | APP11 segments whose JUMBF box is a C2PA manifest store     |
| PNG    | `caBX` chunk                                                |
| WebP   | `C2PA` RIFF chunk                                           |
| all    | `dcterms:provenance` reference in XMP                       |

A store is recognised by its JUMBF type UUID (`c2pa` + `0011-0010-8000-00AA00389B71`),
not by searching for strings, so text that merely mentions "c2pa" is not flagged
and other APP11 users (JPEG XT...) are left alone.

## Usage

```bash
c2pa-strip check photo.jpg pictures/        # exit 1 if C2PA found, 2 on error
c2pa-strip remove photo.jpg                 # writes photo.clean.jpg
c2pa-strip remove -o out.jpg photo.jpg
c2pa-strip remove --in-place -r pictures/
```

`remove` re-checks its output and refuses to write a file that still carries C2PA content.

## Limits

- Invisible watermarks embedded in the pixels (SynthID, TrustMark...) are not metadata and are not touched.
- HEIF/AVIF, MP4 and compressed XMP in PNG are not handled.

## Tests

```bash
cargo test
```

The suite runs on Adobe's sample `tests/fixtures/adobe-20220124-CAICA.jpg`
(checks that the scan data is unchanged byte for byte) plus PNG/WebP files
built on the fly with the same manifest store.
