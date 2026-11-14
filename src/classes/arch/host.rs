//! Objects anchored to another Arch entity.
//!
//! A door, window, opening, or window corner stores an
//! `AEC_ANCHOR_OPENINGBASE_TO_WALL` (or another anchor) whose associative
//! dependency points at the host wall. Arch deletes those anchored
//! entities when the wall is removed.

use crate::entities::{EntityType, ExtendedEntityData};
use crate::objects::{AssociativeData, ObjectType};
use crate::types::Handle;
use crate::CadDocument;
use std::collections::HashSet;

/// Arch entities whose anchor depends on `host`.
pub fn entities_anchored_to(document: &CadDocument, host: Handle) -> Vec<Handle> {
    let mut anchored = Vec::new();
    for object in document.objects.values() {
        let ObjectType::Associative(value) = object else {
            continue;
        };
        let Some(dependent_on) = dependency_target(&value.data) else {
            continue;
        };
        if dependent_on != host {
            continue;
        }
        let Some(entity) = owning_entity(document, value.owner) else {
            continue;
        };
        if entity == host || anchored.contains(&entity) || !is_architecture_entity(document, entity)
        {
            continue;
        }
        anchored.push(entity);
    }
    for object in document.objects.values() {
        let ObjectType::Arch(value) = object else {
            continue;
        };
        if !value.dxf_name.contains("ANCHOR") || value.dxf_name.contains("DISP_REP") {
            continue;
        }
        if !anchor_points_at(value, host) {
            continue;
        }
        let Some(entity) = owning_entity(document, value.handle) else {
            continue;
        };
        if entity == host || anchored.contains(&entity) || !is_architecture_entity(document, entity)
        {
            continue;
        }
        anchored.push(entity);
    }
    anchored
}

fn anchor_points_at(object: &crate::classes::arch::ArchObject, host: Handle) -> bool {
    use crate::io::dwg::dwg_stream_readers::bit_reader::DwgBitReader;
    use crate::io::dwg::dwg_version::DwgVersion;
    use crate::io::dwg::DwgReferenceType;
    use crate::types::DxfVersion;
    let Some(raw) = object.raw_dwg_data.as_deref() else {
        return false;
    };
    if object.raw_dwg_handle_bits <= 8 {
        return false;
    }
    let total_bits = raw.len() as i64 * 8;
    let start = total_bits - object.raw_dwg_handle_bits;
    if start < 0 || start >= total_bits {
        return false;
    }
    let mut reader = DwgBitReader::from_shared(
        std::sync::Arc::<[u8]>::from(raw),
        DwgVersion::AC24,
        object.raw_dwg_version.unwrap_or(DxfVersion::AC1032),
    );
    reader.set_position_in_bits(start);
    let mut reference_type = DwgReferenceType::Undefined;
    for _ in 0..8 {
        if reader.position_in_bits() + 8 > total_bits {
            break;
        }
        let reference = reader.read_handle_reference(object.handle.value(), &mut reference_type);
        if reference == host.value() {
            return true;
        }
        if reference == 0 {
            break;
        }
    }
    false
}

/// Non-graphical objects whose owner chain reaches any of `roots`.
///
/// One pass over the object table for the whole set, so erasing N entities
/// costs O(objects + N) instead of O(objects * N).
pub fn objects_owned_by(document: &CadDocument, roots: &HashSet<Handle>) -> Vec<Handle> {
    document
        .objects
        .keys()
        .copied()
        .filter(|handle| owned_by(document, *handle, roots))
        .collect()
}

fn dependency_target(data: &AssociativeData) -> Option<Handle> {
    match data {
        AssociativeData::Dependency(value) => Some(value.dependent_on),
        AssociativeData::GeomDependency(value) => Some(value.dependency.dependent_on),
        _ => None,
    }
}

fn owning_entity(document: &CadDocument, mut handle: Handle) -> Option<Handle> {
    for _ in 0..24 {
        if document.get_entity(handle).is_some() {
            return Some(handle);
        }
        let owner = document.object_owner(handle)?;
        if owner.is_null() || owner == handle {
            return None;
        }
        handle = owner;
    }
    None
}

#[cfg(test)]
thread_local! {
    /// Owner chains walked, to check that bulk removal scans objects once.
    static OWNER_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn owned_by(document: &CadDocument, mut handle: Handle, roots: &HashSet<Handle>) -> bool {
    #[cfg(test)]
    OWNER_WALKS.with(|n| n.set(n.get() + 1));
    for _ in 0..24 {
        let Some(owner) = document.object_owner(handle) else {
            return false;
        };
        if roots.contains(&owner) {
            return true;
        }
        if owner.is_null() || owner == handle {
            return false;
        }
        handle = owner;
    }
    false
}

pub(crate) fn is_architecture_entity(document: &CadDocument, handle: Handle) -> bool {
    matches!(
        document.get_entity(handle),
        Some(EntityType::Extended(entity))
            if matches!(entity.data, ExtendedEntityData::Arch(_))
    )
}

#[cfg(test)]
mod tests {
    use super::OWNER_WALKS;
    use crate::entities::{EntityType, Line};
    use crate::objects::{ObjectType, XRecord};
    use crate::types::Handle;
    use crate::CadDocument;

    fn xrecord(doc: &mut CadDocument, owner: Handle) -> Handle {
        let mut x = XRecord::new();
        x.handle = doc.allocate_handle();
        x.owner = owner;
        let handle = x.handle;
        doc.objects.insert(handle, ObjectType::XRecord(Box::new(x)));
        handle
    }

    #[test]
    fn bulk_removal_scans_objects_once_and_drops_owned_objects() {
        let mut doc = CadDocument::new();
        let lines: Vec<Handle> = (0..20_000)
            .map(|i| {
                let line = Line::from_coords(i as f64, 0.0, 0.0, i as f64, 1.0, 0.0);
                doc.add_entity(EntityType::Line(line)).unwrap()
            })
            .collect();
        // Objects owned (directly and through a chain) by some lines, plus unrelated ones.
        let owned: Vec<Handle> = lines[..500]
            .iter()
            .flat_map(|&l| {
                let a = xrecord(&mut doc, l);
                [a, xrecord(&mut doc, a)]
            })
            .collect();
        let kept = xrecord(&mut doc, lines[19_999]);
        let free: Vec<Handle> = (0..2_000).map(|_| xrecord(&mut doc, Handle::NULL)).collect();
        let objects = doc.objects.len();

        OWNER_WALKS.with(|n| n.set(0));
        let removed = doc.remove_entities(&lines[..19_999]);
        // One owner walk per object: O(objects + entities), not O(objects * entities).
        assert_eq!(OWNER_WALKS.with(|n| n.get()), objects);
        assert_eq!(removed.len(), 19_999);
        assert_eq!(doc.entities().count(), 1);
        assert!(owned.iter().all(|h| !doc.objects.contains_key(h)));
        assert!(doc.objects.contains_key(&kept));
        assert!(free.iter().all(|h| doc.objects.contains_key(h)));

        // Single removal keeps its behaviour, and a missing handle is a no-op.
        assert!(doc.remove_entity(lines[19_999]).is_some());
        assert!(!doc.objects.contains_key(&kept));
        assert!(doc.remove_entity(lines[0]).is_none());
        assert!(doc.remove_entities(&[free[0]]).is_empty());
        assert!(doc.objects.contains_key(&free[0]));
    }
}
