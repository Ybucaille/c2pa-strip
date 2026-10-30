use std::fs;
use std::process::Command;

use bytes::Bytes;
use c2pa_strip::{inspect, strip};
use img_parts::jpeg::{Jpeg, JpegSegment, markers};
use img_parts::png::{Png, PngChunk};
use img_parts::riff::{RiffChunk, RiffContent};
use img_parts::webp::WebP;
use lopdf::{Document, Object, ObjectId, Stream, dictionary};
use miniz_oxide::deflate::compress_to_vec_zlib;
use miniz_oxide::inflate::decompress_to_vec_zlib;

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
    let found = inspect(&data).unwrap();
    assert!(!found.has_c2pa());
    assert!(!found.has_ai_markers());
}

fn xmp_made_with_ai() -> &'static [u8] {
    br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF><rdf:Description rdf:about="" xmlns:Iptc4xmpExt="http://iptc.org/std/Iptc4xmpExt/2008-02-29/" Iptc4xmpExt:DigitalSourceType="http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia"/></rdf:RDF></x:xmpmeta>"#
}

/// A PNG as written by AUTOMATIC1111, plus IPTC "made with AI" XMP.
fn png_made_with_ai() -> Vec<u8> {
    let mut png = Png::from_bytes(fixture("tiny.png").into()).unwrap();
    let at = png.chunks().len() - 1; // before IEND
    let mut itxt = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
    itxt.extend_from_slice(xmp_made_with_ai());
    png.chunks_mut()
        .insert(at, PngChunk::new(*b"iTXt", itxt.into()));
    png.chunks_mut().insert(
        at,
        PngChunk::new(
            *b"tEXt",
            Bytes::from_static(b"parameters\0a cat, Steps: 20, Sampler: Euler a"),
        ),
    );
    png.encoder().bytes().to_vec()
}

#[test]
fn ai_markers_are_removed() {
    let mut jpeg = Jpeg::from_bytes(fixture("tiny.jpg").into()).unwrap();
    let mut app1 = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    app1.extend_from_slice(xmp_made_with_ai());
    jpeg.segments_mut().insert(
        0,
        JpegSegment::new_with_contents(markers::APP1, app1.into()),
    );
    let jpeg = jpeg.encoder().bytes().to_vec();

    let mut webp = WebP::from_bytes(fixture("tiny.webp").into()).unwrap();
    webp.chunks_mut().push(RiffChunk::new(
        *b"XMP ",
        RiffContent::Data(Bytes::from_static(xmp_made_with_ai())),
    ));
    let webp = webp.encoder().bytes().to_vec();

    let iptc = "IPTC DigitalSourceType trainedAlgorithmicMedia";
    for (name, data, expected) in [
        ("jpeg", jpeg, vec![iptc]),
        (
            "png",
            png_made_with_ai(),
            vec!["PNG text \"parameters\"", iptc],
        ),
        ("webp", webp, vec![iptc]),
    ] {
        let found = inspect(&data).unwrap();
        assert!(!found.has_c2pa(), "{name}");
        assert_eq!(found.ai_markers, expected, "{name}");

        let (out, _) = strip(&data).unwrap();
        assert!(inspect(&out).unwrap().is_empty(), "{name}");
        assert!(
            !out.windows(17).any(|w| w == b"DigitalSourceType"),
            "{name}"
        );
        // The rest of the XMP packet is kept.
        assert!(out.windows(7).any(|w| w == b"rdf:RDF"), "{name}");
    }
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
fn png_compressed_xmp_is_cleaned() {
    let mut png = Png::from_bytes(fixture("tiny.png").into()).unwrap();
    let at = png.chunks().len() - 1; // before IEND
    let xmp = [xmp_with_provenance(), xmp_made_with_ai()].concat();
    let mut itxt = b"XML:com.adobe.xmp\0\x01\x00\0\0".to_vec();
    itxt.extend_from_slice(&compress_to_vec_zlib(&xmp, 6));
    png.chunks_mut()
        .insert(at, PngChunk::new(*b"iTXt", itxt.into()));
    let data = png.encoder().bytes().to_vec();

    let found = inspect(&data).unwrap();
    assert!(found.xmp_provenance);
    assert_eq!(found.ai_markers.len(), 1);

    let (cleaned, _) = strip(&data).unwrap();
    assert!(inspect(&cleaned).unwrap().is_empty());
    let cleaned = Png::from_bytes(cleaned.into()).unwrap();
    let itxt = cleaned.chunk_by_type(*b"iTXt").unwrap().contents().clone();
    assert_eq!(itxt[18], 1, "still compressed");
    let text = decompress_to_vec_zlib(&itxt[22..]).unwrap();
    assert!(text.windows(7).any(|w| w == b"rdf:RDF"));
}

const BMFF_C2PA_UUID: &[u8] = b"\xd8\xfe\xc3\xd6\x1b\x0e\x48\x3c\x92\x97\x58\x28\x87\x7e\xc4\x81";
const BMFF_XMP_UUID: &[u8] = b"\xbe\x7a\xcf\xcb\x97\xa9\x42\xe8\x9c\x71\x99\x94\x91\xe3\xaf\xac";

fn uuid_box(uuid: &[u8], payload: &[u8]) -> Vec<u8> {
    let size = (24 + payload.len()) as u32;
    [&size.to_be_bytes()[..], b"uuid", uuid, payload].concat()
}

/// A C2PA `uuid` box as signers write it: version and flags, purpose,
/// Merkle offset, then the manifest store.
fn c2pa_uuid_box() -> Vec<u8> {
    let payload = [&[0u8; 4][..], b"manifest\0", &[0; 8], &manifest_store_box()].concat();
    uuid_box(BMFF_C2PA_UUID, &payload)
}

/// Inserts `boxes` right after `ftyp` and moves the media data offsets the
/// way a C2PA signer does. Written for the fixtures: `stco` entries, or the
/// single extent of a version 0 `iloc` with 4-byte offsets.
fn insert_after_ftyp(original: &[u8], boxes: &[u8]) -> Vec<u8> {
    let ftyp_end = u32::from_be_bytes(original[..4].try_into().unwrap()) as usize;
    let mut out = [&original[..ftyp_end], boxes, &original[ftyp_end..]].concat();
    let find = |kind: &[u8]| out.windows(4).position(|w| w == kind);
    let mut slots = Vec::new();
    if let Some(p) = find(b"stco") {
        let count = u32::from_be_bytes(out[p + 8..p + 12].try_into().unwrap()) as usize;
        slots.extend((0..count).map(|i| p + 12 + 4 * i));
    }
    if let Some(p) = find(b"iloc") {
        assert_eq!(&out[p + 4..p + 18], b"\0\0\0\0\x44\0\0\x01\0\x01\0\0\0\x01");
        slots.push(p + 18);
    }
    assert!(!slots.is_empty());
    for at in slots {
        let v = u32::from_be_bytes(out[at..at + 4].try_into().unwrap());
        out[at..at + 4].copy_from_slice(&(v + boxes.len() as u32).to_be_bytes());
    }
    out
}

#[test]
fn bmff_c2pa_box_is_removed_and_offsets_restored() {
    for name in ["tiny.mp4", "tiny.mov", "tiny.avif"] {
        let original = fixture(name);
        assert!(inspect(&original).unwrap().is_empty(), "{name}");

        let signed = insert_after_ftyp(&original, &c2pa_uuid_box());
        let found = inspect(&signed).unwrap();
        assert_eq!(found.manifests.len(), 2, "{name}");

        let (cleaned, _) = strip(&signed).unwrap();
        assert_eq!(cleaned, original, "{name}");
    }
}

#[test]
fn bmff_xmp_box_is_cleaned_and_offsets_follow() {
    let original = fixture("tiny.mp4");
    let xmp = String::from_utf8([xmp_with_provenance(), xmp_made_with_ai()].concat()).unwrap();
    let signed = insert_after_ftyp(
        &original,
        &[c2pa_uuid_box(), uuid_box(BMFF_XMP_UUID, xmp.as_bytes())].concat(),
    );
    let found = inspect(&signed).unwrap();
    assert!(found.xmp_provenance);
    assert_eq!(found.ai_markers.len(), 1);

    let clean_xmp = xmp
        .replace(r#" dcterms:provenance="self#jumbf=/c2pa/urn:uuid:1""#, "")
        .replace(
            r#" Iptc4xmpExt:DigitalSourceType="http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia""#,
            "",
        );
    let expected = insert_after_ftyp(&original, &uuid_box(BMFF_XMP_UUID, clean_xmp.as_bytes()));
    let (cleaned, _) = strip(&signed).unwrap();
    assert_eq!(cleaned, expected);
}

/// A one-page PDF, and the id of its catalog.
fn plain_pdf() -> (Document, ObjectId) {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content = Stream::new(dictionary! {}, b"BT 72 720 Td (Hello) Tj ET".to_vec());
    let content_id = doc.add_object(content);
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    (doc, catalog_id)
}

/// Adds what a C2PA signer writes: the manifest store as an embedded file
/// listed in `/AF` and `/EmbeddedFiles`, and the document XMP.
fn sign_pdf(doc: &mut Document, xmp: &[u8]) {
    let file = doc.add_object(Stream::new(
        dictionary! { "Type" => "EmbeddedFile", "Subtype" => "application/c2pa" },
        manifest_store_box(),
    ));
    let name = || Object::string_literal("manifest.c2pa");
    let spec = doc.add_object(dictionary! {
        "Type" => "Filespec",
        "F" => name(),
        "UF" => name(),
        "AFRelationship" => "C2PA_Manifest",
        "EF" => dictionary! { "F" => file },
    });
    let tree = doc.add_object(dictionary! { "Names" => vec![name(), spec.into()] });
    let metadata = doc.add_object(Stream::new(
        dictionary! { "Type" => "Metadata", "Subtype" => "XML" },
        xmp.to_vec(),
    ));
    let catalog = doc.catalog_mut().unwrap();
    catalog.set("AF", vec![Object::Reference(spec)]);
    catalog.set("Names", dictionary! { "EmbeddedFiles" => tree });
    catalog.set("Metadata", metadata);
}

fn pdf_bytes(doc: &mut Document) -> Vec<u8> {
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}

#[test]
fn pdf_manifest_and_ai_markers_are_removed() {
    let (mut plain, catalog_id) = plain_pdf();
    let plain_bytes = pdf_bytes(&mut plain);
    assert!(inspect(&plain_bytes).unwrap().is_empty());

    let mut signed = Document::load_mem(&plain_bytes).unwrap();
    sign_pdf(
        &mut signed,
        &[xmp_with_provenance(), xmp_made_with_ai()].concat(),
    );
    let signed = pdf_bytes(&mut signed);
    let found = inspect(&signed).unwrap();
    assert_eq!(found.manifests.len(), 2);
    assert!(found.xmp_provenance);
    assert_eq!(found.ai_markers.len(), 1);

    let (cleaned, _) = strip(&signed).unwrap();
    assert!(inspect(&cleaned).unwrap().is_empty());
    let store = manifest_store_box();
    assert!(!cleaned.windows(64).any(|w| w == &store[..64]));

    // Every object of the unsigned document is kept as is, pages included;
    // only the catalog changed (it still points to the cleaned XMP).
    let plain = Document::load_mem(&plain_bytes).unwrap();
    let cleaned = Document::load_mem(&cleaned).unwrap();
    for (id, object) in &plain.objects {
        // The cross-reference stream is regenerated on every save.
        if *id != catalog_id && object.type_name().ok() != Some(&b"XRef"[..]) {
            assert!(cleaned.objects.get(id) == Some(object), "{id:?}");
        }
    }
    let catalog = cleaned.catalog().unwrap();
    assert!(catalog.get(b"AF").is_err());
    let tree = catalog
        .get(b"Names")
        .and_then(Object::as_dict)
        .and_then(|n| n.get(b"EmbeddedFiles"))
        .and_then(Object::as_reference)
        .and_then(|id| cleaned.get_dictionary(id))
        .unwrap();
    assert!(tree.get(b"Names").unwrap().as_array().unwrap().is_empty());
}

#[test]
fn pdf_without_c2pa_is_returned_unchanged() {
    let (mut plain, _) = plain_pdf();
    let data = pdf_bytes(&mut plain);
    let (out, removed) = strip(&data).unwrap();
    assert!(removed.is_empty());
    assert_eq!(out, data);
}

#[test]
fn bad_input_is_an_error_not_a_panic() {
    assert!(strip(b"GIF89a....").is_err());
    let adobe = fs::read(ADOBE).unwrap();
    for len in [3, 20, 1000, 70_000] {
        let _ = strip(&adobe[..len]);
    }
    let mp4 = insert_after_ftyp(&fixture("tiny.mp4"), &c2pa_uuid_box());
    for len in 8..mp4.len() {
        let _ = strip(&mp4[..len]);
    }
    let (mut pdf, _) = plain_pdf();
    sign_pdf(&mut pdf, xmp_with_provenance());
    let pdf = pdf_bytes(&mut pdf);
    for len in (5..pdf.len()).step_by(7) {
        let _ = strip(&pdf[..len]);
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

    // AI markers alone make `check` fail, and `remove` drops them.
    let ai = dir.path().join("ai.png");
    fs::write(&ai, png_made_with_ai()).unwrap();
    let ai_s = ai.to_str().unwrap();
    let check = Command::new(bin).args(["check", ai_s]).output().unwrap();
    assert_eq!(check.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&check.stdout).starts_with("AI "));
    assert_eq!(status(&["remove", "--in-place", ai_s]), Some(0));
    assert_eq!(status(&["check", ai_s]), Some(0));
}

/// Files signed in CI by the official c2patool (see tests/e2e/run.sh) from
/// tests/e2e/source.{png,webp}. Stripping must give back the unsigned
/// originals byte for byte.
#[test]
fn c2patool_signed_files_strip_back_to_the_original() {
    for (ext, label) in [
        ("png", "urn:c2pa:ddd5466f-bcb1-4ba9-a895-4b5bb52b5c36"),
        ("webp", "urn:c2pa:b33f0027-7c33-4564-b58a-e6c1ce827b34"),
    ] {
        let signed = fixture(&format!("signed/c2patool-signed.{ext}"));
        let original = fs::read(format!("tests/e2e/source.{ext}")).unwrap();

        let found = inspect(&signed).unwrap();
        assert_eq!(found.manifests, [label], "{ext}");

        let (cleaned, _) = strip(&signed).unwrap();
        assert_eq!(cleaned, original, "{ext}");
    }
}
