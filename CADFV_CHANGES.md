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
| F5/05 | `io/dwg/dwg_writer.rs` | R2000 DWG: `prepare_legacy_document` removed ACAD_MLEADERSTYLE and its style, leaving MLEADERs pointing to nothing (52 AUDIT errors) | The 2004 rule also applies to 2000: MLEADERs and their style are kept, as AutoCAD's SAVEAS 2000 does |
| F5/08 | `io/dwg/dwg_stream_writers/object_writer/mod.rs`, `io/dxf/writer/section_writer.rs`, `io/dxf/reader/section_reader.rs` | An unresolved xref links its nested xref in BLKREFS (DXF group 332 pointing to a BLOCK_RECORD); the DWG writer kept only INSERTs, the DXF writer wrote it as 331 and the reader ignored 332, so AutoCAD purged the nested xref (silent data loss) | BLOCK_RECORDs kept in that list in DWG; written as 332 after the 331 groups in DXF and read back |
| F2c | `xdata/mod.rs` | Merging the `AcDbSavedByObjectVersion` marker into an existing "ACAD" XDATA record replaced the first string by position (VISUALSTYLE `AcRTDxfName…`), so DWG 2004/2000 copies did not open | A marker whose key is already in the record is updated in place |
| D3b | `io/dxf/writer/section_writer.rs` | RAPIDRTRENDERSETTINGS objects written to R2000 DXF (AutoCAD's DXFOUT writes the class but no objects) | Omitted below 2004 and removed from the valid handles so no dictionary entry dangles (also for DATATABLE, D5) |
| D7b | `io/dxf/writer/section_writer.rs`, `io/dxf/reader/section_reader.rs`, `objects/semantic_property.rs` | In DXF before 2007 AutoCAD merges the string stream of 2007+ proxies into the data (data + text + 16-bit size + "has text" bit); `$ACADMAINTVER` 0 made AutoCAD drop objects after proxies; with 20 the proxy 70=1 flag is rejected | `ProxyPayload::merged_with_string_stream` / `split_string_stream`; MAINTVER 20 in R2000 DXF; flag 70 not written in 2000 |
| cotas | `tables/dimstyle.rs` | A new document's Standard dimension style had imperial arrow/text/centre sizes (0.18) while the rest was ISO-25 | ISO-25 values (2.5) and decimal comma for new documents; DXF read defaults keep the imperial ones |
| B-r5 | `io/dxf/writer/section_writer.rs`, `io/dxf/reader/section_reader/table_content.rs` | TABLECONTENT cell values ended with `304 ACVALUE_END` in DXF before 2007 (AutoCAD never writes it there); with `$ACADMAINTVER` 20 AutoCAD rejects the whole R2000 DXF (ErrorStatus=53). The reader also swallowed the following groups when the terminator was missing (AutoCAD's own R2000/R2004 DXF) | New `write_acvalue_end_dxf` writes it only for 2007+; pre-2007 values (group 90 first) end with their typed body |
| Display | `document/explode.rs`, `entities/modification/explode/leaders.rs`, `entities/modification/explode/mod.rs` | Rendering MULTILEADER and TABLE for display needed a read-only explode; EXPLODE dropped the leader landing and outlined the filled arrowhead; tables grew upwards, merged cells showed inner borders and every cell used 2.5 centred text | `explode_for_display` (read-only) keeps landing and filled arrowheads; `explode_table` grows rows downwards from the top-left corner, hides inner borders of merged cells and uses each cell's height, style, alignment and margins |

Results on a 40-file real-world corpus × 8 output formats: rejected DXF 15 → 2, DWG saved in another version that would not open 8 → 2, copies with more AUDIT errors than the original 18 → 5, no copy loses entities.

Update (cadfv 0.0.8-alpha, 2026-10-31): with F2c, D3b, D7b and the R2000 fixes, all copies of the corpus open in AutoCAD 2027; file 08 no longer loses its nested xref.

Update (cadfv 0.0.10-alpha, 2026-10-31): B-r5 and display fixes for multileaders and tables.
