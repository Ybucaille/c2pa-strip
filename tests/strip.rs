use std::fs;
use std::process::Command;

use bytes::Bytes;
use c2pa_strip::{inspect, strip};
use img_parts::jpeg::{Jpeg, JpegSegment, markers};
use img_parts::png::{Png, PngChunk};
use img_parts::riff::{RiffChunk, RiffContent};
use img_parts::webp::WebP;

const ADOBE: &str = "tests/fixtures/adobe-20220124-CAICA.jpg";

fn fixture(name: &str) -> Vec<u8> {
    fs::read(format!("tests/fixtures/{name}")).unwrap()
}

/// Header segments (marker, contents) and the bytes from SOS to the end.
fn split_jpeg(d: &[u8]) -> (Vec<(u8, Vec<u8>)>, Vec<u8>) {
    let mut segs = Vec::new();
    let mut p = 2;
    loop {
        let marker = d[p + 1];
        if marker == markers::SOS {
            return (segs, d[p..].to_vec());
        }
        let len = u16::from_be_bytes([d[p + 2], d[p + 3]]) as usize;
        segs.push((marker, d[p + 4..p + 2 + len].to_vec()));
        p += 2 + len;
    }
}

/// The complete JUMBF manifest store box, reassembled from the Adobe sample.
fn manifest_store_box() -> Vec<u8> {
    let (segs, _) = split_jpeg(&fs::read(ADOBE).unwrap());
    let mut out = Vec::new();
    for (i, (_, c)) in segs
        .iter()
        .filter(|(m, _)| *m == markers::APP11)
        .enumerate()
    {
        out.extend_from_slice(if i == 0 { &c[8..] } else { &c[16..] });
    }
    out
}

fn xmp_with_provenance() -> &'static [u8] {
    br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description rdf:about="" xmlns:dcterms="http://purl.org/dc/terms/" dcterms:provenance="self#jumbf=/c2pa/urn:uuid:1"/></rdf:RDF></x:xmpmeta>"#
}

#[test]
fn adobe_sample_is_detected() {
    let f = inspect(&fs::read(ADOBE).unwrap()).unwrap();
    assert!(f.has_c2pa());
    assert!(f.xmp_provenance);
    assert_eq!(
        f.manifests,
        [
            "contentauth:urn:uuid:04cdf4ec-f713-4e47-a8d6-7af56501ce4b",
            "contentauth:urn:uuid:8f599860-7021-40af-a751-369f6a8bb66e",
        ]
    );
}

#[test]
fn adobe_sample_strip_is_lossless() {
    let original = fs::read(ADOBE).unwrap();
    let (cleaned, removed) = strip(&original).unwrap();
    assert!(removed.has_c2pa());
    assert!(!inspect(&cleaned).unwrap().has_c2pa());

    let (orig_segs, orig_scan) = split_jpeg(&original);
    let (clean_segs, clean_scan) = split_jpeg(&cleaned);
    // Compressed image data untouched, byte for byte.
    assert_eq!(orig_scan, clean_scan);

    // Every header segment kept as is, except the C2PA APP11 ones (gone)
    // and the XMP (same minus the provenance attribute).
    let expected: Vec<_> = orig_segs
        .into_iter()
        .filter(|(m, _)| *m != markers::APP11)
        .collect();
    assert_eq!(expected.len(), clean_segs.len());
    for ((m1, c1), (m2, c2)) in expected.iter().zip(&clean_segs) {
        assert_eq!(m1, m2);
        if *m1 == markers::APP1 && c1.starts_with(b"http://ns.adobe.com/xap/1.0/") {
            let marker = b" dcterms:provenance=\"";
            let start = c1.windows(marker.len()).position(|w| w == marker).unwrap();
            let end = start
                + marker.len()
                + c1[start + marker.len()..]
                    .iter()
                    .position(|&b| b == b'"')
                    .unwrap()
                + 1;
            assert_eq!(&[&c1[..start], &c1[end..]].concat(), c2);
        } else {
            assert_eq!(c1, c2);
        }
    }
}

#[test]
fn strip_is_idempotent() {
    let (once, _) = strip(&fs::read(ADOBE).unwrap()).unwrap();
    let (twice, removed) = strip(&once).unwrap();
    assert!(!removed.has_c2pa());
    assert_eq!(once, twice);
}

#[test]
fn clean_files_are_returned_unchanged() {
    for name in ["tiny.jpg", "tiny.png", "tiny.webp"] {
        let data = fixture(name);
        let (out, removed) = strip(&data).unwrap();
        assert!(!removed.has_c2pa(), "{name}");
        assert_eq!(out, data, "{name}");
    }
}

#[test]
fn non_c2pa_jumbf_in_app11_is_kept() {
    // A JPEG XT style APP11 segment whose JUMBF box has another type UUID.
    let mut jumd = b"jumd".to_vec();
    jumd.extend_from_slice(&[0x11; 16]);
    jumd.extend_from_slice(b"\x03xt\0");
    let mut jumb = Vec::new();
    jumb.extend_from_slice(&(8 + 4 + jumd.len() as u32).to_be_bytes());
    jumb.extend_from_slice(b"jumb");
    jumb.extend_from_slice(&(4 + jumd.len() as u32).to_be_bytes());
    jumb.extend_from_slice(&jumd);
    let mut contents = b"JP\x00\x01\x00\x00\x00\x01".to_vec();
    contents.extend_from_slice(&jumb);

    let mut jpeg = Jpeg::from_bytes(fixture("tiny.jpg").into()).unwrap();
    jpeg.segments_mut().insert(
        1,
        JpegSegment::new_with_contents(markers::APP11, contents.into()),
    );
    let data = jpeg.encoder().bytes().to_vec();

    let (out, removed) = strip(&data).unwrap();
    assert!(!removed.has_c2pa());
    assert_eq!(out, data);
}

#[test]
fn text_mentioning_c2pa_is_not_a_false_positive() {
    // The Go tool flags any "c2pa"/"cai" substring; only real containers count here.
    let mut png = Png::from_bytes(fixture("tiny.png").into()).unwrap();
    let at = png.chunks().len() - 1;
    png.chunks_mut().insert(
        at,
        PngChunk::new(
            *b"tEXt",
            Bytes::from_static(b"Comment\0Caitlin wrote about c2pa and contentauthenticity"),
        ),
    );
    let data = png.encoder().bytes().to_vec();
    assert!(!inspect(&data).unwrap().has_c2pa());
}

#[test]
fn png_cabx_and_xmp_provenance_are_removed() {
    let original = fixture("tiny.png");
    let mut png = Png::from_bytes(original.clone().into()).unwrap();
    let at = png.chunks().len() - 1; // before IEND
    let mut itxt = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
    itxt.extend_from_slice(xmp_with_provenance());
    png.chunks_mut()
        .insert(at, PngChunk::new(*b"iTXt", itxt.into()));
    png.chunks_mut()
        .insert(1, PngChunk::new(*b"caBX", manifest_store_box().into()));
    let data = png.encoder().bytes().to_vec();

    let found = inspect(&data).unwrap();
    assert_eq!(found.manifests.len(), 2);
    assert!(found.xmp_provenance);

    let (cleaned, _) = strip(&data).unwrap();
    assert!(!inspect(&cleaned).unwrap().has_c2pa());
    let cleaned = Png::from_bytes(cleaned.into()).unwrap();
    assert!(cleaned.chunk_by_type(*b"caBX").is_none());
    let xmp = cleaned.chunk_by_type(*b"iTXt").unwrap().contents();
    assert!(!xmp.windows(10).any(|w| w == b"provenance"));
    assert!(xmp.windows(7).any(|w| w == b"rdf:RDF"));
    // Image data chunks untouched.
    let original = Png::from_bytes(original.into()).unwrap();
    assert_eq!(
        original.chunks_by_type(*b"IDAT").collect::<Vec<_>>(),
        cleaned.chunks_by_type(*b"IDAT").collect::<Vec<_>>()
    );
}

#[test]
fn webp_c2pa_chunk_is_removed() {
    let original = fixture("tiny.webp");
    let mut webp = WebP::from_bytes(original.clone().into()).unwrap();
    webp.chunks_mut().push(RiffChunk::new(
        *b"C2PA",
        RiffContent::Data(manifest_store_box().into()),
    ));
    let data = webp.encoder().bytes().to_vec();
    assert_eq!(inspect(&data).unwrap().manifests.len(), 2);

    let (cleaned, _) = strip(&data).unwrap();
    assert_eq!(cleaned, original);
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    assert!(strip(b"GIF89a....").is_err());
    let adobe = fs::read(ADOBE).unwrap();
    for len in [3, 20, 1000, 70_000] {
        let _ = strip(&adobe[..len]);
    }
}

#[test]
fn cli_check_and_remove() {
    let bin = env!("CARGO_BIN_EXE_c2pa-strip");
    let dir = tempfile::tempdir().unwrap();
    let img = dir.path().join("photo.jpg");
    fs::copy(ADOBE, &img).unwrap();

    let status = |args: &[&str]| Command::new(bin).args(args).output().unwrap().status.code();
    let img_s = img.to_str().unwrap();

    assert_eq!(status(&["check", img_s]), Some(1));
    assert_eq!(status(&["check", "tests/fixtures/tiny.png"]), Some(0));
    assert_eq!(status(&["check", "does-not-exist.jpg"]), Some(2));

    assert_eq!(status(&["remove", dir.path().to_str().unwrap()]), Some(0));
    let clean = dir.path().join("photo.clean.jpg");
    assert!(!inspect(&fs::read(&clean).unwrap()).unwrap().has_c2pa());
    // Directory scans skip outputs of earlier runs.
    assert_eq!(status(&["check", dir.path().to_str().unwrap()]), Some(1));

    assert_eq!(status(&["remove", "--in-place", img_s]), Some(0));
    assert_eq!(fs::read(&img).unwrap(), fs::read(&clean).unwrap());
    assert_eq!(status(&["check", img_s]), Some(0));
}
