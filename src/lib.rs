//! Lossless detection and removal of C2PA (Content Credentials) manifests.
//!
//! Only the containers defined by the C2PA specification are touched:
//!
//! - JPEG: the APP11 segments carrying the JUMBF manifest store,
//! - PNG: the `caBX` chunk,
//! - WebP: the `C2PA` RIFF chunk,
//!
//! plus the `dcterms:provenance` reference that C2PA writers add to XMP.
//! Image data, EXIF, ICC profiles and the rest of the XMP are kept byte for
//! byte; nothing is decoded or re-encoded.
//!
//! Generative-AI markers stored outside C2PA (the IPTC digital source type in
//! XMP, the text chunks AI image generators add to PNG) are reported, not
//! removed.

mod jumbf;

use std::collections::{BTreeMap, HashSet};

use anyhow::{Result, bail};
use bytes::Bytes;
use img_parts::jpeg::{Jpeg, JpegSegment, markers};
use img_parts::png::{Png, PngChunk};
use img_parts::riff::RiffChunk;
use img_parts::riff::RiffContent;
use img_parts::webp::WebP;

const JPEG_XMP_PREFIX: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const PNG_XMP_KEYWORD: &[u8] = b"XML:com.adobe.xmp";

/// IPTC digital source types meaning "made with generative AI".
const AI_SOURCE_TYPES: &[&str] = &[
    "trainedAlgorithmicMedia",
    "compositeWithTrainedAlgorithmicMedia",
];
/// PNG text keywords written by AI image generators: AUTOMATIC1111 / Forge
/// (`parameters`), ComfyUI (`prompt`, `workflow`), InvokeAI.
const PNG_AI_KEYWORDS: &[&[u8]] = &[b"parameters", b"prompt", b"workflow", b"invokeai_metadata"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    WebP,
}

impl Format {
    pub fn detect(data: &[u8]) -> Option<Format> {
        if data.starts_with(&[0xFF, 0xD8]) {
            Some(Format::Jpeg)
        } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Format::Png)
        } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            Some(Format::WebP)
        } else {
            None
        }
    }
}

/// What C2PA content a file carries (or carried, after [`strip`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Findings {
    /// Labels of the manifests in the store, e.g. `urn:uuid:...`.
    pub manifests: Vec<String>,
    /// Size in bytes of the containers holding the manifest store.
    pub store_bytes: usize,
    /// Whether XMP holds a `dcterms:provenance` pointer to the manifest.
    pub xmp_provenance: bool,
    /// Generative-AI markers outside C2PA, e.g. `IPTC DigitalSourceType
    /// trainedAlgorithmicMedia`. Reported only: [`strip`] keeps them.
    pub ai_markers: Vec<String>,
}

impl Findings {
    pub fn has_c2pa(&self) -> bool {
        self.store_bytes > 0 || self.xmp_provenance
    }

    pub fn has_ai_markers(&self) -> bool {
        !self.ai_markers.is_empty()
    }
}

/// Reports the C2PA content of an image without modifying it.
pub fn inspect(data: &[u8]) -> Result<Findings> {
    strip(data).map(|(_, findings)| findings)
}

/// Removes the C2PA content of an image. Returns the cleaned file and what
/// was removed; when nothing is found the input is returned unchanged.
pub fn strip(data: &[u8]) -> Result<(Vec<u8>, Findings)> {
    let bytes = Bytes::copy_from_slice(data);
    let (out, findings) = match Format::detect(data) {
        Some(Format::Jpeg) => strip_jpeg(bytes)?,
        Some(Format::Png) => strip_png(bytes)?,
        Some(Format::WebP) => strip_webp(bytes)?,
        None => bail!("unsupported format (expected JPEG, PNG or WebP)"),
    };
    Ok((out.map_or_else(|| data.to_vec(), |b| b.to_vec()), findings))
}

fn strip_jpeg(data: Bytes) -> Result<(Option<Bytes>, Findings)> {
    let mut jpeg = Jpeg::from_bytes(data)?;
    let mut findings = Findings::default();

    // A JUMBF box too large for one segment is split across several APP11
    // segments sharing the same box instance number (En), ordered by the
    // packet sequence number (Z). Each packet starts with "JP", En, Z and
    // then repeats the box header (LBox, TBox[, XLBox]).
    let mut groups: BTreeMap<[u8; 2], Vec<(u32, Bytes)>> = BTreeMap::new();
    for seg in jpeg.segments_by_marker(markers::APP11) {
        let c = seg.contents();
        if c.len() >= 16 && c.starts_with(b"JP") {
            let seq = u32::from_be_bytes(c[4..8].try_into().unwrap());
            groups
                .entry([c[2], c[3]])
                .or_default()
                .push((seq, c.clone()));
        }
    }

    let mut doomed = HashSet::new();
    for (instance, mut packets) in groups {
        packets.sort_by_key(|(seq, _)| *seq);
        let mut payload = Vec::new();
        for (_, packet) in &packets {
            let header = if packet[8..12] == [0, 0, 0, 1] {
                24
            } else {
                16
            };
            if &packet[12..16] != b"jumb" || packet.len() < header {
                payload.clear();
                break;
            }
            payload.extend_from_slice(&packet[header..]);
        }
        if let Some(labels) = jumbf::manifest_store(&payload) {
            doomed.insert(instance);
            findings.manifests.extend(labels);
            findings.store_bytes += packets.iter().map(|(_, p)| p.len() + 4).sum::<usize>();
        }
    }

    let segments = jpeg.segments_mut();
    segments.retain(|seg| {
        let c = seg.contents();
        !(seg.marker() == markers::APP11
            && c.len() >= 16
            && c.starts_with(b"JP")
            && doomed.contains(&[c[2], c[3]]))
    });
    for seg in segments.iter_mut() {
        if seg.marker() != markers::APP1 || !seg.contents().starts_with(JPEG_XMP_PREFIX) {
            continue;
        }
        findings.ai_markers.extend(xmp_ai_markers(seg.contents()));
        if let Some(xmp) = strip_xmp_provenance(seg.contents()) {
            findings.xmp_provenance = true;
            *seg = JpegSegment::new_with_contents(markers::APP1, xmp.into());
        }
    }

    let out = findings.has_c2pa().then(|| jpeg.encoder().bytes());
    Ok((out, findings))
}

fn strip_png(data: Bytes) -> Result<(Option<Bytes>, Findings)> {
    let mut png = Png::from_bytes(data)?;
    let mut findings = Findings::default();

    for chunk in png.chunks_by_type(*b"caBX") {
        findings.store_bytes += chunk.len();
        findings
            .manifests
            .extend(jumbf::find_manifest_store(chunk.contents()).unwrap_or_default());
    }
    png.remove_chunks_by_type(*b"caBX");

    for chunk in png.chunks() {
        if !matches!(&chunk.kind(), b"tEXt" | b"zTXt" | b"iTXt") {
            continue;
        }
        let c = chunk.contents();
        let keyword = &c[..c.iter().position(|&b| b == 0).unwrap_or(c.len())];
        if PNG_AI_KEYWORDS.contains(&keyword) {
            findings
                .ai_markers
                .push(format!("PNG text \"{}\"", String::from_utf8_lossy(keyword)));
        }
    }

    for chunk in png.chunks_mut().iter_mut() {
        if chunk.kind() != *b"iTXt" {
            continue;
        }
        let Some(text_start) = png_xmp_text_offset(chunk.contents()) else {
            continue;
        };
        let (head, text) = chunk.contents().split_at(text_start);
        findings.ai_markers.extend(xmp_ai_markers(text));
        if let Some(xmp) = strip_xmp_provenance(text) {
            findings.xmp_provenance = true;
            *chunk = PngChunk::new(*b"iTXt", [head, &xmp].concat().into());
        }
    }

    let out = findings.has_c2pa().then(|| png.encoder().bytes());
    Ok((out, findings))
}

/// Offset of the text in an uncompressed XMP `iTXt` chunk: keyword NUL,
/// compression flag, compression method, language tag NUL, translated
/// keyword NUL, text.
fn png_xmp_text_offset(itxt: &[u8]) -> Option<usize> {
    let rest = itxt.strip_prefix(PNG_XMP_KEYWORD)?.strip_prefix(b"\0")?;
    if rest.first() != Some(&0) {
        return None; // compressed XMP: left alone
    }
    let mut pos = itxt.len() - rest.len() + 2;
    for _ in 0..2 {
        pos += itxt.get(pos..)?.iter().position(|&b| b == 0)? + 1;
    }
    Some(pos)
}

fn strip_webp(data: Bytes) -> Result<(Option<Bytes>, Findings)> {
    let mut webp = WebP::from_bytes(data)?;
    let mut findings = Findings::default();

    for chunk in webp.chunks_by_id(*b"C2PA") {
        findings.store_bytes += chunk.len() as usize + 8;
        if let Some(data) = chunk.content().data() {
            findings
                .manifests
                .extend(jumbf::find_manifest_store(data).unwrap_or_default());
        }
    }
    webp.remove_chunks_by_id(*b"C2PA");

    for chunk in webp.chunks_mut().iter_mut() {
        if chunk.id() != *b"XMP " {
            continue;
        }
        if let Some(data) = chunk.content().data() {
            findings.ai_markers.extend(xmp_ai_markers(data));
        }
        if let Some(xmp) = chunk.content().data().and_then(|d| strip_xmp_provenance(d)) {
            findings.xmp_provenance = true;
            *chunk = RiffChunk::new(*b"XMP ", RiffContent::Data(xmp.into()));
        }
    }

    let out = findings.has_c2pa().then(|| webp.encoder().bytes());
    Ok((out, findings))
}

/// Removes `dcterms:provenance`, in attribute or element form, from an XMP
/// packet. Returns `None` when the packet has none.
fn strip_xmp_provenance(xmp: &[u8]) -> Option<Vec<u8>> {
    const ATTR: &[u8] = b"dcterms:provenance=";
    const OPEN: &[u8] = b"<dcterms:provenance>";
    const CLOSE: &[u8] = b"</dcterms:provenance>";

    let mut out = xmp.to_vec();
    let mut changed = false;

    while let Some(start) = find(&out, ATTR) {
        let quote = *out.get(start + ATTR.len())?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let value_start = start + ATTR.len() + 1;
        let end = value_start + out[value_start..].iter().position(|&b| b == quote)? + 1;
        // Also drop the whitespace that separated it from the previous attribute.
        let start = out[..start]
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |p| p + 1);
        out.drain(start..end);
        changed = true;
    }
    while let Some(start) = find(&out, OPEN) {
        let end = start + find(&out[start..], CLOSE)? + CLOSE.len();
        out.drain(start..end);
        changed = true;
    }

    changed.then_some(out)
}

/// Lists the generative-AI IPTC digital source types named in an XMP packet
/// (`Iptc4xmpExt:DigitalSourceType`, attribute or element form).
fn xmp_ai_markers(xmp: &[u8]) -> Vec<String> {
    const URI: &[u8] = b"cv.iptc.org/newscodes/digitalsourcetype/";

    let mut markers = Vec::new();
    let mut rest = xmp;
    while let Some(start) = find(rest, URI) {
        rest = &rest[start + URI.len()..];
        let end = rest
            .iter()
            .position(|b| !b.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        let code = String::from_utf8_lossy(&rest[..end]);
        let marker = format!("IPTC DigitalSourceType {code}");
        if AI_SOURCE_TYPES.contains(&&*code) && !markers.contains(&marker) {
            markers.push(marker);
        }
    }
    markers
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_attribute_is_removed() {
        let xmp =
            br#"<rdf:Description rdf:about="" dcterms:provenance="self#jumbf=/c2pa/x" xmp:A="1"/>"#;
        let out = strip_xmp_provenance(xmp).unwrap();
        assert_eq!(out, br#"<rdf:Description rdf:about="" xmp:A="1"/>"#);
    }

    #[test]
    fn xmp_element_is_removed() {
        let xmp = b"<a><dcterms:provenance>self#jumbf=/c2pa</dcterms:provenance></a>";
        assert_eq!(strip_xmp_provenance(xmp).unwrap(), b"<a></a>");
    }

    #[test]
    fn xmp_without_provenance_is_untouched() {
        assert_eq!(strip_xmp_provenance(b"<x:xmpmeta caitlin='c2pa'/>"), None);
    }

    #[test]
    fn xmp_ai_source_types_are_listed() {
        let xmp = br#"<rdf:Description Iptc4xmpExt:DigitalSourceType="http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia"/>
<Iptc4xmpExt:DigitalSourceType>https://cv.iptc.org/newscodes/digitalsourcetype/compositeWithTrainedAlgorithmicMedia</Iptc4xmpExt:DigitalSourceType>
<x Iptc4xmpExt:DigitalSourceType="http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia"/>"#;
        assert_eq!(
            xmp_ai_markers(xmp),
            [
                "IPTC DigitalSourceType trainedAlgorithmicMedia",
                "IPTC DigitalSourceType compositeWithTrainedAlgorithmicMedia",
            ]
        );
    }

    #[test]
    fn xmp_non_ai_source_types_are_ignored() {
        let xmp = br#"<x Iptc4xmpExt:DigitalSourceType="http://cv.iptc.org/newscodes/digitalsourcetype/digitalCapture"/>"#;
        assert!(xmp_ai_markers(xmp).is_empty());
    }
}
