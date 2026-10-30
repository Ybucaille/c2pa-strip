//! ISO base media file format (HEIF, AVIF, MP4, MOV).
//!
//! C2PA stores its manifest store in a top-level `uuid` box, and XMP lives in
//! another one. Removing or shrinking a top-level box shifts every byte after
//! it, so the absolute file offsets kept in `stco`/`co64` (sample chunks),
//! `iloc` (HEIF/AVIF items), `tfhd` and `tfra` (fragments) are patched to
//! match. Anything that cannot be patched safely is an error, never a broken
//! file.

use anyhow::{Result, bail};

use crate::{Findings, clean_xmp, jumbf};

/// `d8fec3d6-1b0e-483c-9297-5828877ec481`
const C2PA_UUID: [u8; 16] = [
    0xd8, 0xfe, 0xc3, 0xd6, 0x1b, 0x0e, 0x48, 0x3c, 0x92, 0x97, 0x58, 0x28, 0x87, 0x7e, 0xc4, 0x81,
];
/// `be7acfcb-97a9-42e8-9c71-999491e3afac`
const XMP_UUID: [u8; 16] = [
    0xbe, 0x7a, 0xcf, 0xcb, 0x97, 0xa9, 0x42, 0xe8, 0x9c, 0x71, 0x99, 0x94, 0x91, 0xe3, 0xaf, 0xac,
];

/// Boxes whose payload is a list of boxes that may hold file offsets.
const CONTAINERS: [&[u8; 4]; 8] = [
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"moof", b"traf", b"mfra",
];

pub fn is_bmff(data: &[u8]) -> bool {
    data.get(4..8) == Some(&b"ftyp"[..])
}

/// A box within the slice it was parsed from.
struct BoxRef {
    start: usize,
    /// Size of the header (size, type, optional 64-bit size).
    header: usize,
    end: usize,
    kind: [u8; 4],
}

impl BoxRef {
    fn uuid<'a>(&self, data: &'a [u8]) -> Option<&'a [u8]> {
        if &self.kind != b"uuid" {
            return None;
        }
        data.get(self.start + self.header..self.start + self.header + 16)
    }
}

/// Parses the boxes laid out back to back in `data[pos..end]`.
fn boxes(data: &[u8], mut pos: usize, end: usize) -> Result<Vec<BoxRef>> {
    let mut out = Vec::new();
    while pos < end {
        if end - pos < 8 {
            bail!("truncated box header");
        }
        let size = read(data, pos, 4)?;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let (header, size) = match size {
            0 => (8, (end - pos) as u64),
            1 => (16, read(data, pos + 8, 8)?),
            n => (8, n),
        };
        if size < header as u64 || size > (end - pos) as u64 {
            bail!("malformed {} box", String::from_utf8_lossy(&kind));
        }
        out.push(BoxRef {
            start: pos,
            header,
            end: pos + size as usize,
            kind,
        });
        pos += size as usize;
    }
    Ok(out)
}

/// A top-level box removed or replaced, in original file positions.
struct Edit {
    start: usize,
    end: usize,
    new_len: usize,
}

/// Removes the C2PA boxes and cleans the XMP box. Returns the new file, or
/// `None` when there is nothing to remove.
pub fn strip(data: &[u8], findings: &mut Findings) -> Result<Option<Vec<u8>>> {
    let top = boxes(data, 0, data.len())?;

    let mut edits = Vec::new();
    let mut replacements = Vec::new();
    for b in &top {
        match b.uuid(data) {
            Some(uuid) if uuid == C2PA_UUID => {
                findings.store_bytes += b.end - b.start;
                findings
                    .manifests
                    .extend(c2pa_box_manifests(&data[b.start + b.header + 16..b.end]));
                edits.push(Edit {
                    start: b.start,
                    end: b.end,
                    new_len: 0,
                });
            }
            Some(uuid) if uuid == XMP_UUID => {
                let Some(xmp) = clean_xmp(&data[b.start + b.header + 16..b.end], findings) else {
                    continue;
                };
                let mut new = Vec::with_capacity(24 + xmp.len());
                new.extend_from_slice(&u32::try_from(24 + xmp.len())?.to_be_bytes());
                new.extend_from_slice(b"uuid");
                new.extend_from_slice(&XMP_UUID);
                new.extend_from_slice(&xmp);
                edits.push(Edit {
                    start: b.start,
                    end: b.end,
                    new_len: new.len(),
                });
                replacements.push((b.start, new));
            }
            _ => {}
        }
    }
    if edits.is_empty() {
        return Ok(None);
    }

    let adjust = |offset: u64| -> Result<u64> {
        let mut shift = 0i128;
        for e in &edits {
            if (e.start as u64..e.end as u64).contains(&offset) {
                bail!("a file offset points into a removed box");
            }
            if e.end as u64 <= offset {
                shift += e.new_len as i128 - (e.end - e.start) as i128;
            }
        }
        Ok(u64::try_from(offset as i128 + shift)?)
    };

    let mut out = Vec::with_capacity(data.len());
    for b in &top {
        if let Some((_, new)) = replacements.iter().find(|(start, _)| *start == b.start) {
            out.extend_from_slice(new);
        } else if edits.iter().any(|e| e.start == b.start) {
            // removed
        } else if CONTAINERS.contains(&&b.kind) || &b.kind == b"meta" {
            let mut copy = data[b.start..b.end].to_vec();
            patch(&mut copy, &adjust)?;
            out.extend_from_slice(&copy);
        } else {
            out.extend_from_slice(&data[b.start..b.end]);
        }
    }
    Ok(Some(out))
}

/// Manifest labels in the payload of a C2PA `uuid` box (after the UUID):
/// version and flags, purpose NUL, then for `manifest` an 8-byte offset to
/// the Merkle data followed by the JUMBF manifest store.
fn c2pa_box_manifests(payload: &[u8]) -> Vec<String> {
    let Some(rest) = payload.get(4..) else {
        return Vec::new();
    };
    let Some(nul) = rest.iter().position(|&b| b == 0) else {
        return Vec::new();
    };
    if &rest[..nul] != b"manifest" {
        return Vec::new();
    }
    rest.get(nul + 1 + 8..)
        .and_then(jumbf::find_manifest_store)
        .unwrap_or_default()
}

/// Patches the file offsets held in `buf`, a whole box, and in its children.
fn patch(buf: &mut [u8], adjust: &dyn Fn(u64) -> Result<u64>) -> Result<()> {
    let b = boxes(buf, 0, buf.len())?.remove(0);
    let h = b.header;
    match &b.kind {
        kind if CONTAINERS.contains(&kind) => patch_children(buf, h, adjust)?,
        // A full box in ISO files, a plain container in QuickTime.
        b"meta" if buf.get(h + 4..h + 8) == Some(&b"hdlr"[..]) => patch_children(buf, h, adjust)?,
        b"meta" => patch_children(buf, h + 4, adjust)?,
        b"stco" | b"co64" => {
            let size = if &b.kind == b"stco" { 4 } else { 8 };
            let count = read(buf, h + 4, 4)? as usize;
            for i in 0..count {
                let p = h + 8 + i * size;
                put(buf, p, size, adjust(read(buf, p, size)?)?)?;
            }
        }
        b"tfhd" => {
            if read(buf, h, 4)? & 1 != 0 {
                put(buf, h + 8, 8, adjust(read(buf, h + 8, 8)?)?)?;
            }
        }
        b"tfra" => {
            let version = read(buf, h, 1)?;
            let sizes = read(buf, h + 8, 4)?;
            let tail = ((sizes >> 4) & 3) + ((sizes >> 2) & 3) + (sizes & 3) + 3;
            let (field, entry) = if version == 1 { (8, 16) } else { (4, 8) };
            let count = read(buf, h + 12, 4)? as usize;
            let mut p = h + 16;
            for _ in 0..count {
                let at = p + field;
                put(buf, at, field, adjust(read(buf, at, field)?)?)?;
                p += entry + tail as usize;
            }
        }
        b"iloc" => patch_iloc(buf, h, adjust)?,
        _ => {}
    }
    Ok(())
}

fn patch_children(buf: &mut [u8], from: usize, adjust: &dyn Fn(u64) -> Result<u64>) -> Result<()> {
    if from > buf.len() {
        bail!("truncated box");
    }
    for child in boxes(buf, from, buf.len())? {
        patch(&mut buf[child.start..child.end], adjust)?;
    }
    Ok(())
}

/// `iloc` item locations with construction method 0 are absolute file
/// offsets: `base_offset + extent_offset`.
fn patch_iloc(buf: &mut [u8], h: usize, adjust: &dyn Fn(u64) -> Result<u64>) -> Result<()> {
    let version = read(buf, h, 1)?;
    let sizes = read(buf, h + 4, 2)?;
    let offset_size = (sizes >> 12) as usize;
    let length_size = ((sizes >> 8) & 15) as usize;
    let base_size = ((sizes >> 4) & 15) as usize;
    let index_size = if version > 0 {
        (sizes & 15) as usize
    } else {
        0
    };
    let id_size = if version < 2 { 2 } else { 4 };
    let count = read(buf, h + 6, id_size)?;
    let mut p = h + 6 + id_size;

    for _ in 0..count {
        p += id_size;
        let method = if version > 0 {
            p += 2;
            read(buf, p - 2, 2)? & 15
        } else {
            0
        };
        let data_ref = read(buf, p, 2)?;
        let base_at = p + 2;
        let base = read(buf, base_at, base_size)?;
        let extents = read(buf, base_at + base_size, 2)?;
        p = base_at + base_size + 2;
        let absolute = method == 0 && data_ref == 0;

        if absolute && offset_size == 0 && base_size > 0 {
            put(buf, base_at, base_size, adjust(base)?)?;
        }
        for _ in 0..extents {
            let at = p + index_size;
            if absolute && offset_size > 0 {
                let offset = read(buf, at, offset_size)?;
                let Some(at_file) = base.checked_add(offset) else {
                    bail!("iloc extent offset overflows");
                };
                let Some(moved) = adjust(at_file)?.checked_sub(base) else {
                    bail!("cannot express a moved iloc extent");
                };
                put(buf, at, offset_size, moved)?;
            }
            p = at + offset_size + length_size;
        }
    }
    Ok(())
}

/// Reads a big-endian unsigned integer of `size` bytes (0 to 8).
fn read(buf: &[u8], at: usize, size: usize) -> Result<u64> {
    let Some(bytes) = buf.get(at..at + size) else {
        bail!("truncated box");
    };
    Ok(bytes.iter().fold(0, |v, &b| v << 8 | b as u64))
}

fn put(buf: &mut [u8], at: usize, size: usize, value: u64) -> Result<()> {
    if size < 8 && value >> (size * 8) != 0 {
        bail!("moved offset does not fit in {size} bytes");
    }
    let Some(bytes) = buf.get_mut(at..at + size) else {
        bail!("truncated box");
    };
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (value >> ((size - 1 - i) * 8)) as u8;
    }
    Ok(())
}
