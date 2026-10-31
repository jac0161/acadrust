# Changes made for cadfv

This branch (`cadfv`) is acadrust **v0.6.8** (tag `v0.6.8`, commit afffe46) with fixes found while validating real-world drawings against AutoCAD 2027 for the cadfv CAD application. Every change is marked in the code with a `cadfv (fix …)` comment. The files remain under the **Mozilla Public License 2.0**, like the rest of acadrust.

Method: each file written by acadrust was opened and audited in AutoCAD 2027 (`accoreconsole` AUDIT + entity count) and compared with AutoCAD's own DXFOUT/SAVEAS of the same original.

| Id | File | Problem | Fix |
|---|---|---|---|
| D1 | `document.rs`, `io/dxf/writer/section_writer.rs` | R14 drawings have no `ACAD_PLOTSTYLENAME`; R2000+ DXF layers then lack group 390 and AutoCAD rejects the file. DEFPOINTS written as plottable is also rejected | New `CadDocument::ensure_plot_style_placeholder()` (creates the dictionary, the "Normal" placeholder and their classes); DEFPOINTS always written non-plottable |
| D2 | `io/dxf/writer/section_writer.rs` | BLOCK_RECORD with an empty name (present in real files; AutoCAD cannot reload even its own DXFOUT of them) | Written as anonymous `*U<handle>` |
| D3 | `io/dxf/writer/section_writer.rs` | RAPIDRTRENDERSETTINGS base class version not decremented for 2018 DXF ("expected group code 290") | Decrement for every target ≥ 2013, as AutoCAD's DXFOUT does |
| D4 | `io/dxf/writer/section_writer.rs` | MTEXT background fill (2004+) written to R2000 DXF | Only written for 2004+ |
| D5 | `io/dxf/writer/section_writer.rs` | DATATABLE written to R2000 DXF (AutoCAD omits it) | Omitted below 2004 |
| D6 | `io/dxf/writer/section_writer.rs` | MTEXT background fill groups out of order / incomplete ("embeddedObject" error) | Written as AutoCAD does: 90, 63, [421], 45, 441 |
| D7 | `io/dxf/writer/section_writer.rs`, `io/dxf/reader/section_reader.rs` | Proxy string stream appended to the 310 data instead of its own 311 groups | Written in 311 after its size; both proxy readers accept 311 (and still the old layout) |
| D4b | `io/dxf/writer/section_writer.rs` | 2018 DXF proxy flagged "from DXF" with DWG binary data is rejected | Flag only written below 2018 |
| F5 | `io/dwg/dwg_stream_writers/object_writer/entities.rs` | VERTEX/SEQEND of POLYLINE and meshes written with ByLayer linetype/lineweight/scale instead of their owner's ("linetype != owner's" AUDIT errors) | New `write_child_entity_data` copies the owner's display properties |
| F5 | `io/dxf/reader.rs` | DXF files from other programs with ownerless OBJECTS entries (AUDIT: "Owner Id Is Null") | `adopt_ownerless_dictionary_entries`: the containing dictionary becomes owner and reactor, as AutoCAD does |

Results on a 40-file real-world corpus × 8 output formats: rejected DXF 15 → 2, DWG saved in another version that would not open 8 → 2, copies with more AUDIT errors than the original 18 → 5, no copy loses entities.
