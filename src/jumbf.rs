//! Minimal JUMBF (ISO/IEC 19566-5) box reader: just enough to recognise a
//! C2PA manifest store and list the manifests it contains.

/// UUID suffix shared by every C2PA JUMBF type (`xxxx-0011-0010-8000-00AA00389B71`).
const C2PA_UUID_SUFFIX: [u8; 12] = [
    0x00, 0x11, 0x00, 0x10, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];

/// A box found while walking a JUMBF byte stream.
pub struct JBox<'a> {
    pub kind: [u8; 4],
    pub payload: &'a [u8],
}

/// Iterates the boxes laid out back to back in `data`. Stops silently at the
/// first truncated or malformed header instead of failing.
pub fn boxes(mut data: &[u8]) -> impl Iterator<Item = JBox<'_>> {
    std::iter::from_fn(move || {
        if data.len() < 8 {
            return None;
        }
        let lbox = u32::from_be_bytes(data[0..4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = data[4..8].try_into().unwrap();
        let (header, size) = match lbox {
            0 => (8, data.len() as u64),
            1 => {
                if data.len() < 16 {
                    return None;
                }
                (16, u64::from_be_bytes(data[8..16].try_into().unwrap()))
            }
            n => (8, n),
        };
        if size < header || size > data.len() as u64 {
            return None;
        }
        let payload = &data[header as usize..size as usize];
        data = &data[size as usize..];
        Some(JBox { kind, payload })
    })
}

/// Description (`jumd`) of a `jumb` superbox: its type UUID and optional label.
pub struct Description {
    pub uuid: [u8; 16],
    pub label: Option<String>,
}

impl Description {
    /// Returns the 4-character C2PA type (`c2pa`, `c2ma`, `c2as`...) if the
    /// UUID belongs to the C2PA family.
    pub fn c2pa_type(&self) -> Option<&[u8]> {
        (self.uuid[4..] == C2PA_UUID_SUFFIX).then(|| &self.uuid[..4])
    }
}

/// Reads the `jumd` box that opens the payload of a `jumb` superbox.
pub fn describe(superbox_payload: &[u8]) -> Option<Description> {
    let jumd = boxes(superbox_payload)
        .next()
        .filter(|b| &b.kind == b"jumd")?;
    let uuid: [u8; 16] = jumd.payload.get(..16)?.try_into().ok()?;
    let toggles = *jumd.payload.get(16)?;
    let label = (toggles & 0x02 != 0).then(|| {
        let rest = &jumd.payload[17..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        String::from_utf8_lossy(&rest[..end]).into_owned()
    });
    Some(Description { uuid, label })
}

/// If `superbox_payload` is a C2PA manifest store, returns the labels of the
/// manifests it holds (possibly empty). Returns `None` for any other box.
pub fn manifest_store(superbox_payload: &[u8]) -> Option<Vec<String>> {
    if describe(superbox_payload)?.c2pa_type()? != b"c2pa" {
        return None;
    }
    let labels = boxes(superbox_payload)
        .skip(1)
        .filter(|b| &b.kind == b"jumb")
        .filter_map(|b| describe(b.payload))
        .filter(|d| matches!(d.c2pa_type(), Some(b"c2ma") | Some(b"c2um")))
        .filter_map(|d| d.label)
        .collect();
    Some(labels)
}

/// Looks for a C2PA manifest store among the top-level boxes of `data`.
pub fn find_manifest_store(data: &[u8]) -> Option<Vec<String>> {
    boxes(data)
        .filter(|b| &b.kind == b"jumb")
        .find_map(|b| manifest_store(b.payload))
}
