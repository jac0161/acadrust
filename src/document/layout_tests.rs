use super::*;

/// After a layout is deleted, a new one takes the first free `*Paper_SpaceN`
/// record and the tab after the last one instead of colliding.
#[test]
fn add_layout_after_deleting_one_uses_a_free_record() {
    let mut doc = CadDocument::new();
    doc.add_layout("Layout2").unwrap();
    doc.add_layout("Layout3").unwrap();
    assert!(doc.block_records.contains("*Paper_Space0") && doc.block_records.contains("*Paper_Space1"));
    // Delete Layout2 (its record and LAYOUT object).
    let gone = doc
        .objects
        .iter()
        .find_map(|(h, o)| matches!(o, ObjectType::Layout(l) if l.name == "Layout2").then_some(*h))
        .unwrap();
    doc.objects.remove(&gone);
    doc.block_records.remove("*Paper_Space0");
    let h = doc.add_layout("Layout4").unwrap();
    let Some(ObjectType::Layout(l)) = doc.objects.get(&h) else { panic!() };
    let record = doc.block_records.iter().find(|b| b.handle == l.block_record).unwrap();
    assert_eq!(record.name, "*Paper_Space0");
    assert_eq!(l.tab_order, 4);
}
