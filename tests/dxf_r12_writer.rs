//! Genuine DXF R12 (AC1009) output via `DxfWriter::with_version`.

use std::io::Cursor;

use acadrust::entities::*;
use acadrust::tables::Layer;
use acadrust::types::{DxfVersion, Vector2, Vector3};
use acadrust::{CadDocument, DxfReader, DxfWriter};

/// (group code, value) pairs of an ASCII DXF.
fn pairs(text: &str) -> Vec<(i32, String)> {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .chunks_exact(2)
        .map(|c| {
            (
                c[0].trim().parse().expect("group code"),
                c[1].trim().to_string(),
            )
        })
        .collect()
}

/// Entity type names (code 0) inside the named section.
fn section_types(pairs: &[(i32, String)], section: &str) -> Vec<String> {
    let mut inside = false;
    let mut types = Vec::new();
    for (i, (code, value)) in pairs.iter().enumerate() {
        if *code == 2 && i > 0 && pairs[i - 1] == (0, "SECTION".to_string()) {
            inside = value == section;
            continue;
        }
        if inside && *code == 0 {
            if value == "ENDSEC" {
                inside = false;
            } else {
                types.push(value.clone());
            }
        }
    }
    types
}

fn header_version(pairs: &[(i32, String)]) -> String {
    let i = pairs
        .iter()
        .position(|p| p == &(9, "$ACADVER".to_string()))
        .expect("$ACADVER");
    pairs[i + 1].1.clone()
}

fn write_r12(doc: &CadDocument) -> String {
    let bytes = DxfWriter::new(doc)
        .with_version(DxfVersion::AC1009)
        .write_to_vec()
        .expect("R12 write");
    String::from_utf8(bytes).expect("ASCII output")
}

fn square_hatch(solid: bool) -> Hatch {
    let mut hatch = if solid { Hatch::solid() } else { Hatch::new() };
    let mut path = BoundaryPath::new();
    let corners = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
    for i in 0..4 {
        let (a, b) = (corners[i], corners[(i + 1) % 4]);
        path.add_edge(BoundaryEdge::Line(LineEdge {
            start: Vector2::new(a.0, a.1),
            end: Vector2::new(b.0, b.1),
        }));
    }
    hatch.add_path(path);
    hatch
}

fn sample_document() -> CadDocument {
    let mut doc = CadDocument::with_version(DxfVersion::AC1032);
    doc.layers.add(Layer::new("My Layer")).unwrap();

    let mut line = Line::from_points(Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 5.0, 0.0));
    line.common.layer = "My Layer".into();
    doc.add_entity(EntityType::Line(line)).unwrap();
    doc.add_entity(EntityType::Circle(Circle::from_center_radius(
        Vector3::new(5.0, 5.0, 0.0),
        2.0,
    )))
    .unwrap();

    let mut pline = LwPolyline::from_points(vec![
        Vector2::new(0.0, 0.0),
        Vector2::new(10.0, 0.0),
        Vector2::new(10.0, 10.0),
    ]);
    pline.vertices[0].bulge = 0.5;
    doc.add_entity(EntityType::LwPolyline(pline)).unwrap();

    let mut mtext = MText::new();
    mtext.value = "{\\fArial|b1;First}\\PSecond".into();
    mtext.height = 2.5;
    doc.add_entity(EntityType::MText(Box::new(mtext))).unwrap();

    let spline = Spline::from_control_points(
        3,
        vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(3.0, 10.0, 0.0),
            Vector3::new(7.0, 10.0, 0.0),
            Vector3::new(10.0, 0.0, 0.0),
        ],
    );
    doc.add_entity(EntityType::Spline(spline)).unwrap();
    doc.add_entity(EntityType::Ellipse(Ellipse::from_center_axes(
        Vector3::ZERO,
        Vector3::new(4.0, 0.0, 0.0),
        0.5,
    )))
    .unwrap();
    doc.add_entity(EntityType::Hatch(Box::new(square_hatch(true))))
        .unwrap();
    doc
}

#[test]
fn r12_output_has_no_r13_structures() {
    let text = write_r12(&sample_document());
    let pairs = pairs(&text);
    assert_eq!(header_version(&pairs), "AC1009");
    let mut record: &str = "";
    for (code, value) in &pairs {
        if *code == 0 {
            record = value.as_str();
        }
        // LinceCAD (fix F-06): an R12 DIMSTYLE carries DIMBLK in group 5 (not a handle);
        // AutoCAD 2027 refuses the record without it.
        if *code == 5 && record == "DIMSTYLE" {
            continue;
        }
        // no handles, owners, subclass markers or application groups
        assert!(
            ![5, 100, 102, 330, 360].contains(code),
            "R13+ group {code} {value}"
        );
    }
    assert!(!pairs.contains(&(2, "CLASSES".into())));
    assert!(!pairs.contains(&(2, "OBJECTS".into())));
    assert!(!pairs.contains(&(2, "BLOCK_RECORD".into())));
}

#[test]
fn newer_entities_are_converted_to_r12_entities() {
    let text = write_r12(&sample_document());
    let pairs = pairs(&text);
    let entities = section_types(&pairs, "ENTITIES");
    for unknown in ["LWPOLYLINE", "MTEXT", "SPLINE", "ELLIPSE", "HATCH"] {
        assert!(
            !entities.iter().any(|t| t == unknown),
            "{unknown} is not an R12 entity: {entities:?}"
        );
    }
    // LWPOLYLINE -> POLYLINE keeping the arc segment
    assert!(
        entities.iter().filter(|t| *t == "POLYLINE").count() >= 3,
        "{entities:?}"
    );
    assert!(pairs.iter().any(|p| p == &(42, "0.5".into())), "bulge kept");
    // MTEXT -> one TEXT per paragraph, formatting removed
    assert!(pairs.contains(&(1, "First".into())) && pairs.contains(&(1, "Second".into())));
    // HATCH -> INSERT of an anonymous *X block filled with SOLIDs
    assert!(entities.iter().any(|t| t == "INSERT"));
    assert!(pairs.contains(&(2, "*X1".into())));
    assert!(section_types(&pairs, "BLOCKS").iter().any(|t| t == "SOLID"));
}

#[test]
fn names_are_valid_r12_symbols() {
    let text = write_r12(&sample_document());
    let pairs = pairs(&text);
    assert!(
        pairs.contains(&(2, "My_Layer".into())),
        "layer table record renamed"
    );
    assert!(
        pairs.contains(&(8, "My_Layer".into())),
        "entity layer renamed consistently"
    );
    assert!(!pairs.iter().any(|(c, v)| *c == 8 && v.contains(' ')));
}

#[test]
fn r12_output_reads_back() {
    let text = write_r12(&sample_document());
    let doc = DxfReader::from_reader(Cursor::new(text.into_bytes()))
        .unwrap()
        .read()
        .unwrap();
    assert_eq!(doc.version, DxfVersion::AC1009);
    let count = |pred: fn(&EntityType) -> bool| doc.entities().filter(|e| pred(e)).count();
    assert_eq!(count(|e| matches!(e, EntityType::Line(_))), 1);
    assert_eq!(count(|e| matches!(e, EntityType::Circle(_))), 1);
    assert_eq!(count(|e| matches!(e, EntityType::Text(_))), 2);
    assert!(doc.layers.get("My_Layer").is_some());
}

#[test]
fn pattern_hatch_is_clipped_to_its_boundary() {
    let mut doc = CadDocument::with_version(DxfVersion::AC1032);
    let mut hatch = square_hatch(false);
    hatch.is_solid = false;
    hatch.pattern.lines.push(HatchPatternLine {
        angle: 0.0,
        base_point: Vector2::new(0.0, 0.0),
        offset: Vector2::new(0.0, 1.0),
        dash_lengths: Vec::new(),
    });
    doc.add_entity(EntityType::Hatch(Box::new(hatch))).unwrap();
    let text = write_r12(&doc);
    let read = DxfReader::from_reader(Cursor::new(text.into_bytes()))
        .unwrap()
        .read()
        .unwrap();
    let block = read.block_records.get("*X1").expect("hatch block");
    let lines: Vec<&Line> = block
        .entity_handles
        .iter()
        .filter_map(|h| match read.get_entity(*h) {
            Some(EntityType::Line(l)) => Some(l),
            _ => None,
        })
        .collect();
    // horizontal lines every unit inside the 10 x 10 square
    assert!(lines.len() >= 9, "{} lines", lines.len());
    for l in lines {
        assert!(
            (l.start.x - 0.0).abs() < 1e-9 && (l.end.x - 10.0).abs() < 1e-9,
            "{l:?}"
        );
    }
}

#[test]
fn documents_carrying_ac1009_are_still_written_as_r13_by_default() {
    let doc = CadDocument::with_version(DxfVersion::AC1009);
    let text = String::from_utf8(DxfWriter::new(&doc).write_to_vec().unwrap()).unwrap();
    assert_eq!(header_version(&pairs(&text)), "AC1012");
}

#[test]
fn an_r12_file_round_trips_as_genuine_r12() {
    let doc = DxfReader::from_file("tests/issue68/r12_mleaderstyle_repro.dxf")
        .unwrap()
        .read()
        .unwrap();
    let text = write_r12(&doc);
    let pairs = pairs(&text);
    assert_eq!(header_version(&pairs), "AC1009");
    let again = DxfReader::from_reader(Cursor::new(text.into_bytes()))
        .unwrap()
        .read()
        .unwrap();
    assert!(again
        .text_styles
        .get("STANDARD")
        .or_else(|| again.text_styles.get("Standard"))
        .is_some());
}

#[test]
fn binary_r12_is_rejected() {
    let doc = sample_document();
    let result = DxfWriter::new_binary(&doc)
        .with_version(DxfVersion::AC1009)
        .write_to_vec();
    assert!(result.is_err());
}
