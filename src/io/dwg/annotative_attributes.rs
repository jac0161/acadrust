//! Recompose INSERTs with annotative attributes from DWG 2000-2007.
//!
//! Observed in what AutoCAD 2027 writes to DWG 2000, 2004 and 2007: an INSERT
//! whose ATTRIBs are annotative is saved "decomposed". The INSERT keeps its
//! handle and layer but references an anonymous block (`*U`), sits at the
//! origin with no scale or rotation, and carries the XDATA
//! `AcadAnnotativeAttributeDecomposition`. That block holds a single INSERT
//! with the real block, placement and ATTRIBs. AutoCAD puts it back together
//! on open (one INSERT of the real block with its ATTRIBs); without this the
//! attributes are only reachable through the anonymous block.
//!
//! The decomposed ATTRIBs keep their `AcadAnnotative` XDATA but with the flag
//! off and an empty `ACDB_ANNOTATIONSCALES`; AutoCAD makes them annotative
//! again with the scales of their ATTDEF, placed where the ATTRIB is.

use crate::document::CadDocument;
use crate::entities::{AttributeEntity, EntityType};
use crate::objects::{MTextAttributeContext, ObjectContextData, ObjectContextKind, ObjectType};
use crate::types::{Handle, Vector2, Vector3};
use crate::xdata::XDataValue;
use std::collections::HashMap;

const APP: &str = "AcadAnnotativeAttributeDecomposition";
const ANNOTATIVE: &str = "AcadAnnotative";

/// Undo the decomposition of every INSERT marked with the XDATA. An INSERT that
/// does not match what AutoCAD writes (moved, scaled, the anonymous block shared
/// or with more than one entity) is left untouched.
pub(crate) fn recompose(doc: &mut CadDocument) {
    let marked: Vec<(Handle, String)> = doc
        .entities()
        .filter_map(|e| match e {
            EntityType::Insert(i) if i.common.extended_data.get_record(APP).is_some() => {
                Some((i.common.handle, i.block_name.clone()))
            }
            _ => None,
        })
        .collect();
    if marked.is_empty() {
        return;
    }
    let mut uses: HashMap<String, usize> = HashMap::new();
    for e in doc.entities() {
        if let EntityType::Insert(i) = e {
            *uses.entry(i.block_name.to_ascii_uppercase()).or_default() += 1;
        }
    }
    let app = doc.app_ids.get(APP).map(|a| a.handle.value());
    for (handle, anonymous) in marked {
        if uses.get(&anonymous.to_ascii_uppercase()) != Some(&1) {
            continue;
        }
        let Some(record) = doc.block_records.get(&anonymous) else { continue };
        if !record.flags.anonymous || record.entity_handles.len() != 1 {
            continue;
        }
        let doomed = [record.entity_handles[0], record.block_entity_handle, record.block_end_handle];
        let Some(EntityType::Insert(inner)) = doc.get_entity(doomed[0]) else { continue };
        let Some(EntityType::Insert(outer)) = doc.get_entity(handle) else { continue };
        let identity = outer.insert_point == Vector3::ZERO
            && outer.rotation == 0.0
            && (outer.x_scale(), outer.y_scale(), outer.z_scale()) == (1.0, 1.0, 1.0)
            && outer.normal == Vector3::new(0.0, 0.0, 1.0)
            && outer.attributes.is_empty();
        if !identity || inner.block_name.eq_ignore_ascii_case(&anonymous) {
            continue;
        }
        let mut joined = inner.clone();
        joined.common = outer.common.clone();
        joined.common.raw_record = None;
        joined.common.extended_data.remove_record(APP);
        if let Some(app) = app {
            joined.common.extended_data.raw_dwg_eed.retain(|(h, _)| *h != app);
        }
        for attribute in &mut joined.attributes {
            attribute.common.owner_handle = handle;
            attribute.common.raw_record = None;
        }
        let block = joined.block_name.clone();
        restore_annotative(doc, &block, &mut joined.attributes);
        if let Some(e) = doc.get_entity_mut(handle) {
            *e = EntityType::Insert(joined);
        }
        for h in doomed {
            doc.remove_entity(h);
        }
        doc.block_records.remove(&anonymous);
    }
}

/// `ACDB_ANNOTATIONSCALES` dictionary under an extension dictionary.
fn scales(doc: &CadDocument, xdic: Option<Handle>) -> Option<Handle> {
    let entry = |h: Handle, key: &str| match doc.objects.get(&h) {
        Some(ObjectType::Dictionary(d)) => d.get(key),
        _ => None,
    };
    entry(entry(xdic?, "AcDbContextDataManager")?, "ACDB_ANNOTATIONSCALES")
}

fn restore_annotative(doc: &mut CadDocument, block: &str, attributes: &mut [AttributeEntity]) {
    let app = doc.app_ids.get(ANNOTATIVE).map(|a| a.handle.value());
    for a in attributes {
        let Some(mut record) = a.common.extended_data.get_record(ANNOTATIVE).cloned() else { continue };
        if let Some(XDataValue::Integer16(flag)) =
            record.values.iter_mut().rev().find(|v| matches!(v, XDataValue::Integer16(_)))
        {
            *flag = 1;
        }
        a.common.extended_data.upsert_record(record);
        // The writer prefers the verbatim bytes, which still say "off".
        if let Some(app) = app {
            a.common.extended_data.raw_dwg_eed.retain(|(h, _)| *h != app);
        }
        let Some(target) = scales(doc, a.common.xdictionary_handle) else { continue };
        if !matches!(doc.objects.get(&target), Some(ObjectType::Dictionary(d)) if d.entries.is_empty()) {
            continue;
        }
        let definition = doc.entities_in_block(block).find_map(|e| match e {
            EntityType::AttributeDefinition(d) if d.tag == a.tag => Some(d.common.xdictionary_handle),
            _ => None,
        });
        let Some(source) = definition.and_then(|x| scales(doc, x)) else { continue };
        let Some(ObjectType::Dictionary(source)) = doc.objects.get(&source) else { continue };
        let contexts: Vec<ObjectContextData> = source
            .entries
            .iter()
            .filter_map(|(_, h)| match doc.objects.get(h) {
                Some(ObjectType::ObjectContextData(c)) => Some((**c).clone()),
                _ => None,
            })
            .collect();
        for (i, mut c) in contexts.into_iter().enumerate() {
            let h = doc.allocate_handle();
            (c.handle, c.owner_handle, c.reactors, c.xdictionary_handle) = (h, target, vec![target], None);
            c.kind = ObjectContextKind::MTextAttribute(MTextAttributeContext {
                horizontal_mode: a.horizontal_alignment as i16,
                rotation: a.rotation,
                insertion: Vector2::new(a.insertion_point.x, a.insertion_point.y),
                alignment: Vector2::new(a.alignment_point.x, a.alignment_point.y),
                enable_context: false,
                context: None,
            });
            doc.context_scales.insert(h, c.scale);
            doc.objects.insert(h, ObjectType::ObjectContextData(Box::new(c)));
            if let Some(ObjectType::Dictionary(d)) = doc.objects.get_mut(&target) {
                d.add_entry(format!("*A{}", i + 1), h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::entities::EntityType;

    /// AutoCAD 2027 drawing saved as DWG 2000: block BLQ with an annotative
    /// ATTDEF (TAGAN) and an annotative multiline one (TAGMA, M1\PM2\PM3),
    /// inserted once at 100,0.
    #[test]
    fn autocad_r2000_annotative_attributes_are_recomposed() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/annotative/autocad_2027_r2000_attributes.dwg"
        ));
        let doc = crate::io::dwg::DwgReader::from_stream(std::io::Cursor::new(&bytes[..]))
            .read()
            .expect("the multiline annotative context must decode");
        let inserts: Vec<_> = doc
            .model_space_entities()
            .filter_map(|e| match e {
                EntityType::Insert(i) => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(inserts.len(), 1);
        let i = inserts[0];
        assert_eq!((i.block_name.as_str(), i.insert_point.x), ("BLQ", 100.0));
        assert!(i.common.extended_data.get_record(super::APP).is_none());
        let tags: Vec<_> = i.attributes.iter().map(|a| (a.tag.as_str(), a.value.as_str())).collect();
        assert_eq!(tags, [("TAGMA", "M1"), ("TAGAN", "ValorAN"), ("TAGMA_002", "M2"), ("TAGMA_003", "M3")]);
        assert!(i.attributes.iter().all(|a| a.common.owner_handle == i.common.handle));
        // Annotative again, with the scale of its ATTDEF where the ATTRIB is.
        let a = &i.attributes[1];
        let flag = a.common.extended_data.get_record("AcadAnnotative").and_then(|r| r.values.iter().rev().find_map(|v| match v {
            crate::xdata::XDataValue::Integer16(f) => Some(*f),
            _ => None,
        }));
        assert_eq!(flag, Some(1));
        let s = super::scales(&doc, a.common.xdictionary_handle).unwrap();
        let Some(crate::objects::ObjectType::Dictionary(s)) = doc.objects.get(&s) else { panic!() };
        assert_eq!(s.entries.len(), 1);
        let Some(crate::objects::ObjectType::ObjectContextData(c)) = doc.objects.get(&s.entries[0].1) else { panic!() };
        let crate::objects::ObjectContextKind::MTextAttribute(k) = &c.kind else { panic!() };
        assert_eq!((k.insertion.x, k.insertion.y), (105.0, -10.0));
        assert!(!doc.block_records.iter().any(|b| b.flags.anonymous && b.name.starts_with("*U")));
    }
}
