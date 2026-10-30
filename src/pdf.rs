//! PDF.
//!
//! C2PA embeds the manifest store as an embedded file: a file specification
//! with `/AFRelationship /C2PA_Manifest` (the stream itself typed
//! `application/c2pa`), listed in the catalog's `/AF` array and usually in
//! the `/EmbeddedFiles` name tree, added by an incremental update.
//!
//! Removing it means rewriting the file as a single revision: page content
//! streams and every other object are kept as they are, but the file is not
//! byte-identical to the original. Earlier revisions, which may still hold
//! older manifests, are dropped with it.

use std::collections::HashSet;

use anyhow::{Result, bail};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use crate::{Findings, clean_xmp, jumbf};

pub fn is_pdf(data: &[u8]) -> bool {
    data.starts_with(b"%PDF-")
}

/// Removes the C2PA manifest store and cleans the document XMP. Returns the
/// rewritten file, or `None` when there is nothing to remove.
pub fn strip(data: &[u8], findings: &mut Findings) -> Result<Option<Vec<u8>>> {
    let mut doc = Document::load_mem(data)?;
    if doc.was_encrypted() {
        bail!("encrypted PDF files are not supported");
    }

    let mut doomed = HashSet::new();
    for (&id, object) in &doc.objects {
        let Ok(spec) = object.as_dict() else {
            continue;
        };
        let files = embedded_files(spec);
        let is_c2pa = name_is(spec, b"AFRelationship", b"C2PA_Manifest")
            || files.iter().any(|&file| {
                stream(&doc, file)
                    .is_some_and(|s| name_is(&s.dict, b"Subtype", b"application/c2pa"))
            });
        if !is_c2pa {
            continue;
        }
        for &file in &files {
            if let Some(content) = stream(&doc, file).and_then(|s| s.get_plain_content().ok()) {
                findings.store_bytes += content.len();
                findings
                    .manifests
                    .extend(jumbf::find_manifest_store(&content).unwrap_or_default());
            }
        }
        doomed.insert(id);
        doomed.extend(files);
    }

    let mut xmp_changed = false;
    let metadata = doc
        .catalog()
        .and_then(|c| c.get(b"Metadata"))
        .and_then(Object::as_reference);
    if let Ok(Object::Stream(xmp)) = metadata.and_then(|id| doc.get_object_mut(id)) {
        let content = xmp.get_plain_content()?;
        if let Some(clean) = clean_xmp(&content, findings) {
            xmp.set_plain_content(clean);
            xmp_changed = true;
        }
    }

    if doomed.is_empty() && !xmp_changed {
        return Ok(None);
    }
    for id in &doomed {
        doc.objects.remove(id);
    }
    for object in doc.objects.values_mut() {
        unlink(object, &doomed);
    }

    let mut out = Vec::new();
    doc.save_to(&mut out)?;
    Ok(Some(out))
}

/// Streams referenced from a file specification's `/EF` dictionary.
fn embedded_files(spec: &Dictionary) -> Vec<ObjectId> {
    let Ok(ef) = spec.get(b"EF").and_then(Object::as_dict) else {
        return Vec::new();
    };
    ef.iter()
        .filter_map(|(_, v)| v.as_reference().ok())
        .collect()
}

fn stream(doc: &Document, id: ObjectId) -> Option<&Stream> {
    doc.get_object(id).and_then(Object::as_stream).ok()
}

fn name_is(dict: &Dictionary, key: &[u8], name: &[u8]) -> bool {
    dict.get(key)
        .and_then(Object::as_name)
        .is_ok_and(|n| n == name)
}

fn is_doomed(object: &Object, doomed: &HashSet<ObjectId>) -> bool {
    matches!(object, Object::Reference(id) if doomed.contains(id))
}

/// Drops every reference to a removed object: from arrays such as `/AF`,
/// from dictionaries, and from name trees, where the key goes with it.
fn unlink(object: &mut Object, doomed: &HashSet<ObjectId>) {
    match object {
        Object::Array(items) => {
            items.retain(|o| !is_doomed(o, doomed));
            for item in items {
                unlink(item, doomed);
            }
        }
        Object::Dictionary(dict) => unlink_dict(dict, doomed),
        Object::Stream(stream) => unlink_dict(&mut stream.dict, doomed),
        _ => {}
    }
}

fn unlink_dict(dict: &mut Dictionary, doomed: &HashSet<ObjectId>) {
    // A name tree leaf: [key1 value1 key2 value2 ...].
    if let Ok(Object::Array(names)) = dict.get_mut(b"Names")
        && names.len() % 2 == 0
    {
        *names = names
            .chunks(2)
            .filter(|pair| !is_doomed(&pair[1], doomed))
            .flatten()
            .cloned()
            .collect();
    }
    let keys: Vec<Vec<u8>> = dict
        .iter()
        .filter(|(_, v)| is_doomed(v, doomed))
        .map(|(k, _)| k.clone())
        .collect();
    for key in keys {
        dict.remove(&key);
    }
    for (_, value) in dict.iter_mut() {
        unlink(value, doomed);
    }
    if dict
        .get(b"AF")
        .and_then(Object::as_array)
        .is_ok_and(|af| af.is_empty())
    {
        dict.remove(b"AF");
    }
}
