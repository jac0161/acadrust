//! lince (deshacer por diferencias): what changed between two states of a document, keeping only the changed
//! items of one side. [`DocumentDelta::swap`] exchanges them with the document, so the same delta undoes and
//! redoes: applied to the `to` state it yields `from` and then holds what `to` had.
//!
//! Every field of [`CadDocument`] is covered (the destructuring in [`DocumentDelta::between`] stops compiling
//! when a field is added): whole values for scalars and small structs, per key for the handle maps, per position
//! for the entity storage (its `Arc`s are compared by pointer, so unchanged geometry costs nothing) and for the
//! symbol tables.

use super::CadDocument;
use crate::entities::EntityType;
use crate::tables::{BlockRecord, Table, TableEntry};
use crate::types::Handle;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};
use std::mem::size_of;
use std::sync::Arc;

type Part = Box<dyn FnMut(&mut CadDocument) + Send>;

/// Positions of an ordered sequence that differ, with this side's values (`None`: beyond this side's length).
struct Positions<T> {
    len: usize,
    /// Length of the other side: what the sequence is expected to have when swapped.
    other: usize,
    items: Vec<(usize, Option<T>)>,
}

impl<T> Positions<T> {
    fn new<R: Copy>(from: &[R], to: &[R], same: impl Fn(R, R) -> bool, own: impl Fn(R) -> T) -> Self {
        let items = (0..from.len().max(to.len()))
            .filter(|&i| match (from.get(i), to.get(i)) {
                (Some(a), Some(b)) => !same(*a, *b),
                _ => true,
            })
            .map(|i| (i, from.get(i).map(|a| own(*a))))
            .collect();
        Positions { len: from.len(), other: to.len(), items }
    }

    /// Puts the stored values into `v` and keeps the ones they replace. Items appended after this delta was taken
    /// (a change that did not go through undo) are kept at the end and returned with their new position; a shorter
    /// sequence than expected is left untouched (`None`) rather than guessed at.
    fn swap(&mut self, v: &mut Vec<T>) -> Option<Vec<usize>> {
        if v.len() < self.other {
            return None;
        }
        let extra: Vec<T> = v.drain(self.other..).collect();
        let (cur, new) = (v.len(), self.len);
        // Every position in new..cur is listed, in order: what goes beyond the new length comes off the end.
        let mut tail: Vec<Option<T>> = if new < cur { v.drain(new..).map(Some).collect() } else { Vec::new() };
        for (i, slot) in &mut self.items {
            let i = *i;
            *slot = match slot.take() {
                Some(x) if i < cur => Some(std::mem::replace(&mut v[i], x)),
                Some(x) => {
                    debug_assert_eq!(v.len(), i);
                    v.push(x);
                    None
                }
                None => (i < cur).then(|| tail[i - new].take()).flatten(),
            };
        }
        (self.len, self.other) = (cur, new);
        let moved = (v.len()..v.len() + extra.len()).collect();
        v.extend(extra);
        Some(moved)
    }
}

/// The changes between two states of a document (see the module docs).
pub struct DocumentDelta {
    entities: Positions<Arc<EntityType>>,
    parts: Vec<Part>,
    weight: usize,
}

impl std::fmt::Debug for DocumentDelta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DocumentDelta({} entity positions, {} parts, ~{} bytes)", self.entities.items.len(), self.parts.len(), self.weight)
    }
}

fn whole<T: PartialEq + Clone + Send + 'static>(from: &T, to: &T, get: fn(&mut CadDocument) -> &mut T, parts: &mut Vec<Part>, w: &mut usize) {
    if from != to {
        let mut v = from.clone();
        *w += size_of::<T>();
        parts.push(Box::new(move |d| std::mem::swap(get(d), &mut v)));
    }
}

fn map<K, V, S>(from: &HashMap<K, V, S>, to: &HashMap<K, V, S>, get: fn(&mut CadDocument) -> &mut HashMap<K, V, S>, parts: &mut Vec<Part>, w: &mut usize)
where
    K: Hash + Eq + Clone + Send + 'static,
    V: PartialEq + Clone + Send + 'static,
    S: BuildHasher + Send + 'static,
{
    let mut v: Vec<(K, Option<V>)> = from.iter().filter(|(k, x)| to.get(*k) != Some(*x)).map(|(k, x)| (k.clone(), Some(x.clone()))).collect();
    v.extend(to.keys().filter(|k| !from.contains_key(*k)).map(|k| (k.clone(), None)));
    if v.is_empty() {
        return;
    }
    // shortcut: size of the stored items without what they own on the heap; count it if the budget falls short.
    *w += v.len() * size_of::<(K, Option<V>)>();
    parts.push(Box::new(move |d| {
        let m = get(d);
        for (k, x) in &mut v {
            *x = match x.take() {
                Some(val) => m.insert(k.clone(), val),
                None => m.remove(k),
            };
        }
    }));
}

fn table<T>(from: &Table<T>, to: &Table<T>, get: fn(&mut CadDocument) -> &mut Table<T>, extra: fn(&T) -> usize, parts: &mut Vec<Part>, w: &mut usize)
where
    T: TableEntry + PartialEq + Clone + Send + 'static,
{
    let (a, b) = (&from.entries, &to.entries);
    let mut v: Vec<(String, Option<T>)> = a.iter().filter(|(k, x)| b.get(*k) != Some(*x)).map(|(k, x)| (k.clone(), Some(x.clone()))).collect();
    v.extend(b.keys().filter(|k| !a.contains_key(*k)).map(|k| (k.clone(), None)));
    // The order of the entries only when it changed (an entry removed and put back goes at the end otherwise).
    let mut order: Option<Vec<String>> = (!a.keys().eq(b.keys())).then(|| a.keys().cloned().collect());
    let mut handle = from.handle;
    if v.is_empty() && order.is_none() && from.handle == to.handle {
        return;
    }
    *w += v.iter().map(|(_, x)| size_of::<(String, Option<T>)>() + x.as_ref().map_or(0, extra)).sum::<usize>();
    *w += order.as_ref().map_or(0, |o| o.len() * size_of::<String>());
    parts.push(Box::new(move |d| {
        let t = get(d);
        std::mem::swap(&mut t.handle, &mut handle);
        let now: Option<Vec<String>> = order.as_ref().map(|_| t.entries.keys().cloned().collect());
        for (k, x) in &mut v {
            *x = match x.take() {
                Some(val) => t.entries.insert(k.clone(), val),
                None => t.entries.shift_remove(k),
            };
        }
        if let (Some(o), Some(now)) = (order.as_mut(), now) {
            let pos: HashMap<&String, usize> = o.iter().enumerate().map(|(i, k)| (k, i)).collect();
            // Entries the delta does not know (added outside undo) go last, in their order.
            t.entries.sort_by_cached_key(|k, _| pos.get(k).copied().unwrap_or(usize::MAX));
            *o = now;
        }
    }));
}

/// Rough heap of an entity (the vertex lists that dominate big drawings).
fn entity_heap(e: &EntityType) -> usize {
    match e {
        EntityType::LwPolyline(p) => p.vertices.len() * 40,
        EntityType::Polyline(p) => p.vertices.len() * 64,
        EntityType::Spline(s) => (s.control_points.len() + s.fit_points.len()) * 24 + s.knots.len() * 8,
        EntityType::MText(t) => t.value.len(),
        EntityType::Text(t) => t.value.len(),
        _ => 0,
    }
}

fn block_record_heap(b: &BlockRecord) -> usize {
    (b.entity_handles.len() + b.insert_handles.len()) * 8 + b.preview_data.len() + b.insert_count_bytes.len()
}

impl DocumentDelta {
    /// What has to change in `to` to get `from` back (`from`'s side of every difference).
    pub fn between(from: &CadDocument, to: &CadDocument) -> Self {
        let (mut parts, mut w): (Vec<Part>, usize) = (Vec::new(), 0);
        macro_rules! fields {
            (whole: $($a:ident),*; maps: $($m:ident),*; tables: $($t:ident: $x:expr),*) => {
                // Exhaustive on purpose: a new document field must be added here.
                let CadDocument { entities: _, entity_index: _, $($a: _,)* $($m: _,)* $($t: _,)* } = from;
                $(whole(&from.$a, &to.$a, |d| &mut d.$a, &mut parts, &mut w);)*
                $(map(&from.$m, &to.$m, |d| &mut d.$m, &mut parts, &mut w);)*
                $(table(&from.$t, &to.$t, |d| &mut d.$t, $x, &mut parts, &mut w);)*
            };
        }
        fields!(
            whole: version, maintenance_version, header, vx_control_entries, classes, notifications, summary_info, source_path,
                dwg_source_version, preview, acis_sab_handles, raw_acds_data, r12_header_raw, raw_acds_fingerprint,
                dwg_data_store_handles, section_view_style, section_view_reps, next_handle;
            maps: objects, block_visibility_params, context_scales, block_representations, fields, dgn_ls_definitions,
                dgn_ls_components, eed_by_handle, object_xdata, xdic_by_handle, reactors_by_handle, block_entity_handles,
                r12_table_usage, r12_block_tags, raw_records, original_objects, raw_record_owners, view_rep_refs;
            tables: layers: |_| 0, line_types: |_| 0, text_styles: |_| 0, block_records: block_record_heap, dim_styles: |_| 0,
                app_ids: |_| 0, views: |_| 0, vports: |_| 0, ucss: |_| 0, vx_table: |_| 0
        );
        let (a, b): (Vec<&Arc<EntityType>>, Vec<&Arc<EntityType>>) = (from.entities.iter().collect(), to.entities.iter().collect());
        let entities = Positions::new(&a, &b, |x, y| Arc::ptr_eq(x, y), |x| Arc::clone(x));
        // The `from` versions no longer in `to` live on only here (an entity that just moved position is shared).
        let moved: std::collections::HashSet<*const EntityType> =
            entities.items.iter().filter_map(|(i, _)| to.entities.get(*i)).map(Arc::as_ptr).collect();
        w += entities.items.len() * size_of::<(usize, Option<Arc<EntityType>>)>();
        w += entities.items.iter().filter_map(|(_, x)| x.as_ref()).filter(|x| !moved.contains(&Arc::as_ptr(x))).map(|x| size_of::<EntityType>() + entity_heap(x)).sum::<usize>();
        DocumentDelta { entities, parts, weight: w }
    }

    /// Nothing differs.
    pub fn is_empty(&self) -> bool {
        self.entities.items.is_empty() && self.parts.is_empty()
    }

    /// Approximate memory it holds, in bytes.
    pub fn weight(&self) -> usize {
        self.weight
    }

    /// Handles of `doc`'s entities at the positions this delta changes (call before [`swap`](Self::swap)).
    pub fn current_handles(&self, doc: &CadDocument) -> Vec<Handle> {
        self.entities.items.iter().filter_map(|(i, _)| doc.entities.get(*i)).map(|e| e.common().handle).collect()
    }

    /// Exchanges the stored side with `doc` (undo, and again redo). Returns the entities that were at the changed
    /// positions before (some only moved: compare by pointer) and the handles that are there now.
    pub fn swap(&mut self, doc: &mut CadDocument) -> (Vec<Arc<EntityType>>, Vec<Handle>) {
        for p in &mut self.parts {
            p(doc);
        }
        let Some(moved) = self.entities.swap(&mut doc.entities) else {
            // shortcut: entities removed outside undo since this step; the step leaves them alone instead of guessing.
            eprintln!("acadrust: undo step skipped the entities ({} expected, {} found)", self.entities.other, doc.entities.len());
            return (Vec::new(), Vec::new());
        };
        for i in moved {
            doc.entity_index.insert(doc.entities[i].common().handle, i);
        }
        let before: Vec<Arc<EntityType>> = self.entities.items.iter().filter_map(|(_, x)| x.clone()).collect();
        for (i, e) in self.entities.items.iter().filter_map(|(i, x)| Some((*i, x.as_ref()?))) {
            let h = e.common().handle;
            if doc.entity_index.get(&h) == Some(&i) {
                doc.entity_index.remove(&h);
            }
        }
        let mut after = Vec::with_capacity(self.entities.items.len());
        for &(i, _) in &self.entities.items {
            if let Some(e) = doc.entities.get(i) {
                let h = e.common().handle;
                doc.entity_index.insert(h, i);
                after.push(h);
            }
        }
        (before, after)
    }
}

impl CadDocument {
    /// lince (memoria al abrir): frees the spare capacity the readers leave in the entity storage and its handle
    /// index (with the 487 991-entity test drawing, room for 2.7 M entities and 3.7 M index entries: 71 MB, and the
    /// index keeps its capacity in every clone of the document).
    pub fn shrink_storage(&mut self) {
        self.entities.shrink_to_fit();
        self.entity_index.shrink_to_fit();
    }
}

/// lince (memoria al abrir): bytes vivos por campo de un dibujo grande, con un asignador que cuenta. Ignorada:
/// `ACADRUST_PLANO=plano.dwg cargo test --release -p acadrust --lib memory_by_field -- --ignored --nocapture`.
#[cfg(test)]
mod memory {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};

    struct Counting;
    static LIVE: AtomicIsize = AtomicIsize::new(0);
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            LIVE.fetch_add(l.size() as isize, Relaxed);
            unsafe { System.alloc(l) }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            LIVE.fetch_sub(l.size() as isize, Relaxed);
            unsafe { System.dealloc(p, l) }
        }
    }
    #[global_allocator]
    static A: Counting = Counting;

    #[test]
    #[ignore]
    fn memory_by_field() {
        let path = std::env::var("ACADRUST_PLANO").expect("ACADRUST_PLANO");
        let mb = |x: isize| x as f64 / 1e6;
        let start = LIVE.load(Relaxed);
        let mut doc = crate::DwgReader::from_file(&path).and_then(|mut r| r.read()).unwrap();
        let total = LIVE.load(Relaxed) - start;
        println!("document: {:.0} MB live after reading", mb(total));
        println!("entity_index: {} entries, capacity {}; entities capacity {}", doc.entity_index.len(), doc.entity_index.capacity(), doc.entities.capacity());
        let b = LIVE.load(Relaxed);
        doc.shrink_storage();
        println!("shrink_to_fit of the index and the entity vector: {:.1} MB", mb(LIVE.load(Relaxed) - b));
        let mut rows = Vec::new();
        macro_rules! free {
            ($($f:ident),*) => {$(
                let b = LIVE.load(Relaxed);
                drop(std::mem::take(&mut doc.$f));
                rows.push((LIVE.load(Relaxed) - b, stringify!($f)));
            )*};
        }
        free!(objects, eed_by_handle, object_xdata, xdic_by_handle, reactors_by_handle, block_entity_handles, raw_records,
            original_objects, raw_acds_data, block_visibility_params, context_scales, fields, view_rep_refs, layers,
            block_records, text_styles, line_types, dim_styles, entity_index);
        let b = LIVE.load(Relaxed);
        let n = doc.entities.len();
        drop(std::mem::take(&mut doc.entities));
        rows.push((LIVE.load(Relaxed) - b, "entities"));
        let b = LIVE.load(Relaxed);
        drop(doc);
        rows.push((LIVE.load(Relaxed) - b, "rest"));
        rows.sort();
        for (x, f) in rows {
            println!("{f:<24} {:>9.1} MB", -mb(x));
        }
        println!("{n} entities");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::{Circle, Line};
    use crate::tables::Layer;
    use crate::types::Vector3;

    fn line(x: f64) -> EntityType {
        EntityType::Line(Line::from_points(Vector3::new(x, 0.0, 0.0), Vector3::new(x, 1.0, 0.0)))
    }

    #[test]
    fn undo_and_redo_restore_both_states_exactly() {
        let mut doc = CadDocument::new();
        let hs: Vec<Handle> = (0..6).map(|i| doc.add_entity(line(i as f64)).unwrap()).collect();
        let from = doc.clone();
        // Remove (swap_remove reorders), modify, add, a new layer and a header change.
        doc.remove_entity(hs[1]);
        if let Some(EntityType::Line(l)) = doc.get_entity_mut(hs[4]) {
            l.start.x = 99.0;
        }
        doc.add_entity(EntityType::Circle(Circle::new())).unwrap();
        doc.layers.add(Layer::new("Muros")).unwrap();
        doc.header.current_layer_name = "Muros".into();
        let to = doc.clone();

        let mut d = DocumentDelta::between(&from, &doc);
        assert!(!d.is_empty() && d.weight() > 0);
        let (before, after) = d.swap(&mut doc);
        assert!(doc == from, "undo restores the earlier state");
        assert!(before.iter().any(|e| e.common().handle != hs[1]) && after.contains(&hs[1]));
        for h in &hs {
            assert!(doc.get_entity(*h).is_some(), "index rebuilt for {h:?}");
        }
        d.swap(&mut doc);
        assert!(doc == to, "redo restores the later state");
        assert!(doc.get_entity(hs[1]).is_none());
        assert!(DocumentDelta::between(&doc, &to).is_empty());

        // An entity added outside undo survives undo and redo.
        let extra = doc.add_entity(line(50.0)).unwrap();
        d.swap(&mut doc);
        assert!(doc.get_entity(extra).is_some() && doc.get_entity(hs[1]).is_some());
        assert!(doc.layers.get("Muros").is_none());
        d.swap(&mut doc);
        assert!(doc.get_entity(extra).is_some() && doc.get_entity(hs[1]).is_none());
        // Removed outside undo: the step leaves the entities alone instead of corrupting them.
        doc.remove_entity(extra);
        doc.remove_entity(hs[0]);
        let (a, b) = d.swap(&mut doc);
        assert!(a.is_empty() && b.is_empty() && doc.get_entity(hs[2]).is_some());
    }
}
