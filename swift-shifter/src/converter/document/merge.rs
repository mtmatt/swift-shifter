use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Walk the page tree upward from `page_id` and collect attributes that pages
/// inherit from their parent /Pages nodes (/MediaBox, /Resources, /Rotate,
/// /CropBox). Returns a Dictionary of inherited values not present on the page.
fn apply_inherited_to_page(doc: &Document, page_id: ObjectId) -> Dictionary {
    // Keys that PDF spec says are inheritable (PDF 1.7 §7.7.3.4)
    const INHERITABLE: &[&[u8]] = &[b"MediaBox", b"Resources", b"Rotate", b"CropBox"];

    // Start from the immediate parent of the page
    let parent_id = match doc.objects.get(&page_id) {
        Some(obj) => match obj.as_dict() {
            Ok(dict) => match dict.get(b"Parent") {
                Ok(Object::Reference(id)) => *id,
                _ => return Dictionary::new(),
            },
            Err(_) => return Dictionary::new(),
        },
        None => return Dictionary::new(),
    };

    let mut seen_keys: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
    let mut inherited: Vec<(Vec<u8>, Object)> = Vec::new();

    let mut current_id = parent_id;
    let mut visited: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();
    loop {
        if !visited.insert(current_id) {
            break; // cycle detected in parent chain
        }
        let dict = match doc.objects.get(&current_id) {
            Some(obj) => match obj.as_dict() {
                Ok(d) => d,
                Err(_) => break,
            },
            None => break,
        };

        for key in INHERITABLE {
            if !seen_keys.contains(*key) {
                if let Ok(val) = dict.get(*key) {
                    seen_keys.insert(key.to_vec());
                    inherited.push((key.to_vec(), val.clone()));
                }
            }
        }

        match dict.get(b"Parent") {
            Ok(Object::Reference(id)) => current_id = *id,
            _ => break,
        }
    }

    let mut result = Dictionary::new();
    for (key, val) in inherited {
        result.set(key, val);
    }
    result
}

/// Renumber all objects in `doc` so that every object ID starts at `base`.
/// Returns the remapping table (old_id → new_id).
fn renumber_objects(doc: &Document, base: u32) -> (BTreeMap<ObjectId, Object>, BTreeMap<ObjectId, ObjectId>, u32) {
    // Build a remapping table: old id → new id
    let mut id_map: BTreeMap<ObjectId, ObjectId> = BTreeMap::new();
    let mut counter = base;
    for &old_id in doc.objects.keys() {
        id_map.insert(old_id, (counter, 0));
        counter += 1;
    }

    // Clone + rewrite all Reference objects in place
    let new_objects: BTreeMap<ObjectId, Object> = doc.objects
        .iter()
        .map(|(&old_id, obj)| {
            let new_id = id_map[&old_id];
            let new_obj = rewrite_refs(obj, &id_map);
            (new_id, new_obj)
        })
        .collect();

    (new_objects, id_map, counter)
}

/// Deep-clone an Object, replacing every Reference with its remapped ID.
fn rewrite_refs(obj: &Object, id_map: &BTreeMap<ObjectId, ObjectId>) -> Object {
    match obj {
        Object::Reference(id) => {
            if let Some(&new_id) = id_map.get(id) {
                Object::Reference(new_id)
            } else {
                Object::Reference(*id)
            }
        }
        Object::Array(arr) => Object::Array(
            arr.iter().map(|o| rewrite_refs(o, id_map)).collect(),
        ),
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (k, v) in dict.iter() {
                new_dict.set(k.clone(), rewrite_refs(v, id_map));
            }
            Object::Dictionary(new_dict)
        }
        Object::Stream(stream) => {
            let mut new_dict = Dictionary::new();
            for (k, v) in stream.dict.iter() {
                new_dict.set(k.clone(), rewrite_refs(v, id_map));
            }
            Object::Stream(lopdf::Stream::new(new_dict, stream.content.clone()))
        }
        other => other.clone(),
    }
}

/// Merge two or more PDFs in the given order.
/// Output is named `{first_stem}-merged.pdf` next to the first file,
/// or inside `output_dir` if supplied.
/// Returns the output path string on success.
pub fn merge_pdfs(input_paths: &[String], output_dir: Option<&str>) -> Result<String, String> {
    if input_paths.len() < 2 {
        return Err("Need at least 2 PDFs to merge".to_string());
    }

    // Derive output path from the first input
    let out_path: PathBuf = {
        let p = Path::new(&input_paths[0]);
        let stem = p.file_stem().unwrap_or_default().to_string_lossy();
        let dir = match output_dir {
            Some(d) => {
                let dir = PathBuf::from(d);
                std::fs::create_dir_all(&dir)
                    .map_err(|e| format!("Failed to create output dir: {e}"))?;
                dir
            }
            None => p.parent().unwrap_or(Path::new(".")).to_path_buf(),
        };
        dir.join(format!("{}-merged.pdf", stem))
    };

    // Load all source documents
    let docs: Vec<Document> = input_paths
        .iter()
        .map(|p| Document::load(p).map_err(|e| format!("Cannot read '{}': {e}", p)))
        .collect::<Result<_, _>>()?;

    // Renumber every document so their object IDs don't collide, then
    // collect page IDs (after remapping) and all objects into one pool.
    let mut next_base: u32 = 1;
    let mut all_page_ids: Vec<ObjectId> = Vec::new();
    let mut merged_objects: BTreeMap<ObjectId, Object> = BTreeMap::new();

    for doc in &docs {
        // Collect page ids from the original document before renumbering
        let original_page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();

        // For each page, collect attributes it would inherit from its parent
        // /Pages node (e.g. /MediaBox, /Resources). Must happen before
        // renumbering because the parent chain only exists in the original doc.
        let inherited_by_page: HashMap<ObjectId, Dictionary> = original_page_ids
            .iter()
            .map(|&pid| (pid, apply_inherited_to_page(doc, pid)))
            .collect();

        let (mut new_objects, id_map, new_next) = renumber_objects(doc, next_base);

        // Map the original page ids to their new ids
        for orig_page_id in original_page_ids {
            if let Some(&new_page_id) = id_map.get(&orig_page_id) {
                all_page_ids.push(new_page_id);

                // Copy inherited attributes onto the page if it doesn't already
                // define them explicitly. This preserves /MediaBox etc. that
                // were set on the source /Pages node rather than the page itself.
                if let Some(inherited) = inherited_by_page.get(&orig_page_id) {
                    if let Some(obj) = new_objects.get_mut(&new_page_id) {
                        if let Ok(dict) = obj.as_dict_mut() {
                            for (key, val) in inherited.iter() {
                                if dict.get(key.as_slice()).is_err() {
                                    dict.set(key.clone(), rewrite_refs(val, &id_map));
                                }
                            }
                        }
                    }
                }
            }
        }

        merged_objects.extend(new_objects);
        next_base = new_next;
    }

    // Build the merged document from scratch
    let mut out = Document::with_version("1.7");
    out.objects = merged_objects;
    out.max_id = next_base - 1;

    // Create a new root Pages node that owns every page
    let pages_id: ObjectId = (out.max_id + 1, 0);
    out.max_id += 1;

    // Update every page's /Parent to point at the new Pages node
    for &page_id in &all_page_ids {
        if let Some(obj) = out.objects.get_mut(&page_id) {
            if let Ok(dict) = obj.as_dict_mut() {
                dict.set("Parent", Object::Reference(pages_id));
            }
        }
    }

    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set(
        "Kids",
        Object::Array(
            all_page_ids
                .iter()
                .map(|&id| Object::Reference(id))
                .collect(),
        ),
    );
    pages_dict.set("Count", Object::Integer(all_page_ids.len() as i64));
    out.objects.insert(pages_id, Object::Dictionary(pages_dict));

    // Create a new Catalog pointing at the new Pages node
    let catalog_id: ObjectId = (out.max_id + 1, 0);
    out.max_id += 1;
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    out.objects.insert(catalog_id, Object::Dictionary(catalog));

    out.trailer.set("Root", Object::Reference(catalog_id));
    out.trailer.set("Size", Object::Integer(out.max_id as i64 + 1));

    out.save(&out_path).map_err(|e| e.to_string())?;
    Ok(out_path.to_string_lossy().into_owned())
}
