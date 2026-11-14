//! lince (PROXY-CIVIL3D): the proxy-graphics metafile as plain entities.
//!
//! Record layout from the Open Design Specification for .dwg files
//! ("proxy entity graphics"): little-endian 32-bit counts, doubles in WCS,
//! strings null-terminated and padded to 4 bytes.

use super::proxy_graphics::{ProxyGraphicRecord, ProxyGraphics};
use super::{Arc, Circle, EntityCommon, EntityType, Hatch, Polyline3D, Ray, Text, XLine};
use crate::types::{Color, LineWeight, Matrix4, Transform, Vector2, Vector3};

const MAX_POINTS: usize = 1_000_000;
const MAX_MATRIX_DEPTH: usize = 64;

/// What the metafile's trait records refer to by index.
#[derive(Debug, Clone, Copy)]
pub struct ProxyDrawContext<'a> {
    /// Layer names in layer-table order (trait 16 indexes them).
    pub layers: &'a [String],
    /// Linetype names in linetype-table order (trait 18 indexes them).
    pub linetypes: &'a [String],
    /// Layer of the proxy entity: the one primitives use until a layer trait.
    pub layer: &'a str,
    /// True for a metafile from DWG 2007+ (type-10/11 text is UTF-16 there).
    pub unicode: bool,
}

struct State {
    common: EntityCommon,
    fill: bool,
    matrices: Vec<Matrix4>,
}

impl ProxyGraphics {
    /// The metafile as plain entities (lines, polylines, circles, arcs,
    /// texts, solid fills…) in WCS, with the colour, layer, linetype and
    /// lineweight traits applied. Colour, linetype and lineweight start as
    /// BYBLOCK so they follow the proxy entity, as AutoCAD draws them.
    /// Records that draw nothing in plan (extents, clipping, materials…)
    /// are skipped.
    pub fn to_entities(&self, context: &ProxyDrawContext<'_>) -> Vec<EntityType> {
        let mut common = EntityCommon::new();
        common.layer = context.layer.to_string();
        common.color = Color::ByBlock;
        common.linetype = "ByBlock".to_string();
        common.line_weight = LineWeight::ByBlock;
        let mut state = State { common, fill: false, matrices: Vec::new() };
        let mut output = Vec::new();
        for record in &self.records {
            match record {
                ProxyGraphicRecord::FillOff => state.fill = false,
                ProxyGraphicRecord::UnicodeText(t) => {
                    push_text(&mut output, &state, [t.position, t.normal, t.direction], [t.height, t.width_factor, t.oblique_angle], t.text.clone())
                }
                ProxyGraphicRecord::Unknown { record_type, data } => draw(&mut output, &mut state, context, *record_type, data),
            }
        }
        output
    }
}

fn draw(output: &mut Vec<EntityType>, state: &mut State, context: &ProxyDrawContext<'_>, record_type: u32, data: &[u8]) {
    let v = |offset: usize| read_vector(data, offset);
    let f = |offset: usize| read_f64(data, offset);
    match record_type {
        // CIRCLE: center, radius, normal.
        2 => {
            if let (Some(center), Some(radius), Some(normal)) = (v(0), f(24), v(32)) {
                push_circle(output, state, center, radius, normal);
            }
        }
        // CIRCLE 3P.
        3 => {
            if let Some((center, radius, normal)) = v(0).zip(v(24)).zip(v(48)).and_then(|((a, b), c)| circle_3p(a, b, c)) {
                push_circle(output, state, center, radius, normal);
            }
        }
        // CIRCULAR ARC: center, radius, normal, start vector, sweep angle, arc type.
        4 => {
            let (Some(center), Some(radius), Some(normal), Some(start), Some(sweep)) = (v(0), f(24), v(32), v(56), f(80)) else { return };
            let normal = unit(normal);
            let s = to_ocs(normal, start);
            push_arc(output, state, center, radius, normal, s.y.atan2(s.x), sweep, read_u32(data, 88).unwrap_or(0));
        }
        // CIRCULAR ARC 3P: start, a point on the arc, end, arc type.
        5 => {
            let (Some(a), Some(b), Some(c)) = (v(0), v(24), v(48)) else { return };
            let Some((center, radius, normal)) = circle_3p(a, b, c) else {
                push_polyline(output, state, vec![a, c], false);
                return;
            };
            let angle = |p: Vector3| {
                let q = to_ocs(normal, p - center);
                q.y.atan2(q.x)
            };
            let start = angle(a);
            let sweep = (angle(c) - start).rem_euclid(std::f64::consts::TAU);
            push_arc(output, state, center, radius, normal, start, sweep, read_u32(data, 72).unwrap_or(0));
        }
        // POLYLINE, POLYGON and POLYLINE WITH NORMALS: count, points.
        6 | 7 | 32 => {
            let Some(points) = read_points(data) else { return };
            if record_type == 7 && state.fill && points.len() >= 3 {
                push_fill(output, state, &points);
            } else {
                push_polyline(output, state, points, record_type == 7);
            }
        }
        // MESH: rows, columns, points; drawn as its wireframe.
        8 => {
            let (Some(rows), Some(columns)) = (read_u32(data, 0), read_u32(data, 4)) else { return };
            let (rows, columns) = (rows as usize, columns as usize);
            let Some(count) = rows.checked_mul(columns).filter(|n| *n <= MAX_POINTS) else { return };
            let Some(points) = (0..count).map(|i| v(8 + i * 24)).collect::<Option<Vec<_>>>() else { return };
            for row in 0..rows {
                push_polyline(output, state, points[row * columns..(row + 1) * columns].to_vec(), false);
            }
            for column in 0..columns {
                push_polyline(output, state, (0..rows).map(|row| points[row * columns + column]).collect(), false);
            }
        }
        // SHELL: points, then the face list (vertex count and indices; a negative count is a hole).
        9 => {
            let Some(points) = read_points(data) else { return };
            let mut offset = 4 + points.len() * 24;
            let Some(entries) = read_u32(data, offset) else { return };
            offset += 4;
            let end = offset.saturating_add((entries as usize).saturating_mul(4)).min(data.len());
            while offset + 4 <= end {
                let Some(n) = read_u32(data, offset).map(|n| (n as i32).unsigned_abs() as usize) else { return };
                offset += 4;
                let Some(face) = (0..n).map(|i| read_u32(data, offset + i * 4).and_then(|k| points.get(k as usize).copied())).collect::<Option<Vec<_>>>() else {
                    return;
                };
                offset += n * 4;
                if state.fill && face.len() >= 3 {
                    push_fill(output, state, &face);
                } else {
                    push_polyline(output, state, face, true);
                }
            }
        }
        // TEXT: position, normal, direction, height, width factor, oblique angle, string.
        10 => {
            let (Some(p), Some(n), Some(d), Some(h), Some(w), Some(o)) = (v(0), v(24), v(48), f(72), f(80), f(88)) else { return };
            if let Some((text, _)) = read_padded_string(data, 96, context.unicode) {
                push_text(output, state, [p, n, d], [h, w, o], text);
            }
        }
        // TEXT2 and UNICODE TEXT2: position, normal, direction, string, length, raw flag, height, width factor,
        // oblique angle, tracking, flags and font names.
        11 | 38 => {
            let (Some(p), Some(n), Some(d)) = (v(0), v(24), v(48)) else { return };
            let Some((text, next)) = read_padded_string(data, 72, record_type == 38 || context.unicode) else { return };
            if let (Some(h), Some(w), Some(o)) = (f(next + 8), f(next + 16), f(next + 24)) {
                push_text(output, state, [p, n, d], [h, w, o], text);
            }
        }
        // XLINE and RAY: two points.
        12 | 13 => {
            let (Some(a), Some(b)) = (v(0), v(24)) else { return };
            if (b - a).length() > f64::EPSILON {
                let entity = if record_type == 12 { EntityType::XLine(XLine::from_points(a, b)) } else { EntityType::Ray(Ray::from_points(a, b)) };
                push(output, state, entity);
            }
        }
        // Traits: colour, layer, linetype, fill, true colour, lineweight, linetype scale.
        14 => {
            if let Some(index) = read_u32(data, 0) {
                state.common.color = if index <= 256 { Color::from_index(index as i16) } else { Color::ByLayer };
            }
        }
        16 => {
            if let Some(name) = read_u32(data, 0).and_then(|i| context.layers.get(i as usize)) {
                state.common.layer = name.clone();
            }
        }
        18 => {
            let name = match read_u32(data, 0) {
                Some(32767) => Some("ByLayer".to_string()),
                Some(32766) => Some("ByBlock".to_string()),
                Some(i) => context.linetypes.get(i as usize).cloned(),
                None => None,
            };
            if let Some(name) = name {
                state.common.linetype = name;
            }
        }
        20 => state.fill = read_u32(data, 0).is_some_and(|v| v != 0),
        22 => {
            let color = read_u32(data, 0).and_then(|raw| match raw >> 24 {
                0xC0 => Some(Color::ByLayer),
                0xC1 => Some(Color::ByBlock),
                0xC2 => Some(Color::from_true_color_value((raw & 0xFF_FFFF) as i32)),
                0xC3 => Some(Color::from_index((raw & 0xFF) as i16)),
                _ => None,
            });
            if let Some(color) = color {
                state.common.color = color;
            }
        }
        23 => {
            if let Some(value) = read_u32(data, 0) {
                state.common.line_weight = LineWeight::from_value(value as i32 as i16);
            }
        }
        24 => {
            if let Some(scale) = f(0).filter(|s| *s > 0.0) {
                state.common.linetype_scale = scale;
            }
        }
        // PUSH MATRIX: row-major 4x4 with the translation in the last column; it applies before the current one.
        29 | 30 => {
            let Some(values) = (0..16).map(|i| f(i * 8)).collect::<Option<Vec<_>>>() else { return };
            if state.matrices.len() >= MAX_MATRIX_DEPTH {
                return;
            }
            let mut m = Matrix4::identity();
            for (i, value) in values.into_iter().enumerate() {
                m.m[i / 4][i % 4] = value;
            }
            let combined = state.matrices.last().map_or(m, |top| *top * m);
            state.matrices.push(combined);
        }
        31 => {
            state.matrices.pop();
        }
        _ => {}
    }
}

/// A null-terminated string padded to 4 bytes: the text and the offset after the padding.
fn read_padded_string(data: &[u8], offset: usize, unicode: bool) -> Option<(String, usize)> {
    let tail = data.get(offset..)?;
    let (text, used) = if unicode {
        let (pares, _) = tail.as_chunks::<2>();
        let end = pares.iter().position(|c| *c == [0, 0])?;
        let units: Vec<u16> = pares[..end].iter().map(|c| u16::from_le_bytes(*c)).collect();
        (String::from_utf16_lossy(&units), end * 2 + 2)
    } else {
        let end = tail.iter().position(|b| *b == 0)?;
        // shortcut: bytes above 127 are read as Latin-1 (ANSI_1252 drawings); other code pages need DWGCODEPAGE.
        (tail[..end].iter().map(|b| char::from(*b)).collect(), end + 1)
    };
    Some((text, offset + used.div_ceil(4) * 4))
}

fn read_points(data: &[u8]) -> Option<Vec<Vector3>> {
    let count = read_u32(data, 0)? as usize;
    if count > MAX_POINTS || 4 + count * 24 > data.len() {
        return None;
    }
    (0..count).map(|i| read_vector(data, 4 + i * 24)).collect()
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
}

fn read_f64(data: &[u8], offset: usize) -> Option<f64> {
    let value = f64::from_le_bytes(data.get(offset..offset + 8)?.try_into().ok()?);
    value.is_finite().then_some(value)
}

fn read_vector(data: &[u8], offset: usize) -> Option<Vector3> {
    Some(Vector3::new(read_f64(data, offset)?, read_f64(data, offset + 8)?, read_f64(data, offset + 16)?))
}

fn unit(normal: Vector3) -> Vector3 {
    if normal.length() > 1e-12 {
        normal.normalize()
    } else {
        Vector3::UNIT_Z
    }
}

/// X and Y axes of the object coordinate system of `normal` (the arbitrary axis algorithm).
fn ocs_axes(normal: Vector3) -> (Vector3, Vector3, Vector3) {
    let n = unit(normal);
    let reference = if n.x.abs() < 1.0 / 64.0 && n.y.abs() < 1.0 / 64.0 { Vector3::UNIT_Y } else { Vector3::UNIT_Z };
    let x = reference.cross(&n).normalize();
    (x, n.cross(&x).normalize(), n)
}

fn to_ocs(normal: Vector3, p: Vector3) -> Vector3 {
    let (x, y, n) = ocs_axes(normal);
    Vector3::new(p.dot(&x), p.dot(&y), p.dot(&n))
}

fn from_ocs(normal: Vector3, p: Vector3) -> Vector3 {
    let (x, y, n) = ocs_axes(normal);
    x * p.x + y * p.y + n * p.z
}

fn circle_3p(a: Vector3, b: Vector3, c: Vector3) -> Option<(Vector3, f64, Vector3)> {
    let (ab, ac) = (b - a, c - a);
    let normal = ab.cross(&ac);
    let n2 = normal.dot(&normal);
    if n2 <= 1e-24 {
        return None;
    }
    let center = a + (normal.cross(&ab) * ac.dot(&ac) + ac.cross(&normal) * ab.dot(&ab)) * (0.5 / n2);
    Some((center, (a - center).length(), normal.normalize()))
}

/// Adds `entity`, built in the metafile's coordinates, with the current traits and matrix.
fn push(output: &mut Vec<EntityType>, state: &State, mut entity: EntityType) {
    let common = entity.common_mut();
    common.layer = state.common.layer.clone();
    common.color = state.common.color;
    common.linetype = state.common.linetype.clone();
    common.line_weight = state.common.line_weight;
    common.linetype_scale = state.common.linetype_scale;
    if let Some(m) = state.matrices.last() {
        entity.as_entity_mut().apply_transform(&Transform::from_matrix(*m));
    }
    output.push(entity);
}

fn push_polyline(output: &mut Vec<EntityType>, state: &State, points: Vec<Vector3>, closed: bool) {
    if points.len() >= 2 {
        let mut polyline = Polyline3D::from_points(points);
        polyline.flags.closed = closed;
        push(output, state, EntityType::Polyline3D(polyline));
    }
}

fn push_fill(output: &mut Vec<EntityType>, state: &State, points: &[Vector3]) {
    use super::hatch::{BoundaryEdge, BoundaryPath, PolylineEdge};
    let normal = points.windows(3).map(|w| (w[1] - w[0]).cross(&(w[2] - w[0]))).find(|n| n.length() > 1e-12).map_or(Vector3::UNIT_Z, unit);
    let local: Vec<Vector3> = points.iter().map(|p| to_ocs(normal, *p)).collect();
    let mut hatch = Hatch::solid();
    hatch.normal = normal;
    hatch.elevation = local[0].z;
    let mut path = BoundaryPath::external();
    path.add_edge(BoundaryEdge::Polyline(PolylineEdge::new(local.iter().map(|p| Vector2::new(p.x, p.y)).collect(), true)));
    hatch.add_path(path);
    push(output, state, EntityType::Hatch(Box::new(hatch)));
}

fn push_circle(output: &mut Vec<EntityType>, state: &State, center: Vector3, radius: f64, normal: Vector3) {
    if radius > 0.0 {
        let normal = unit(normal);
        let mut circle = Circle::from_center_radius(to_ocs(normal, center), radius);
        circle.normal = normal;
        push(output, state, EntityType::Circle(circle));
    }
}

/// Arc of `sweep` radians from `start` (OCS angle); arc types 1 (sector) and 2 (chord) close it with lines.
#[allow(clippy::too_many_arguments)]
fn push_arc(output: &mut Vec<EntityType>, state: &State, center: Vector3, radius: f64, normal: Vector3, start: f64, sweep: f64, arc_type: u32) {
    if radius <= 0.0 {
        return;
    }
    let (start, end) = if sweep < 0.0 { (start + sweep, start) } else { (start, start + sweep) };
    let mut arc = Arc::new();
    (arc.center, arc.radius, arc.normal, arc.start_angle, arc.end_angle) = (to_ocs(normal, center), radius, normal, start, end);
    let c = arc.center;
    let ends = [start, end].map(|a| from_ocs(normal, Vector3::new(c.x + radius * a.cos(), c.y + radius * a.sin(), c.z)));
    push(output, state, EntityType::Arc(arc));
    match arc_type {
        1 => push_polyline(output, state, vec![ends[0], center, ends[1]], false),
        2 => push_polyline(output, state, ends.to_vec(), false),
        _ => {}
    }
}

fn push_text(output: &mut Vec<EntityType>, state: &State, [position, normal, direction]: [Vector3; 3], [height, width, oblique]: [f64; 3], text: String) {
    if text.is_empty() || !(height.is_finite() && height > 0.0) {
        return;
    }
    let normal = unit(normal);
    let d = to_ocs(normal, direction);
    let mut entity = Text::with_value(text, to_ocs(normal, position)).with_height(height).with_rotation(d.y.atan2(d.x));
    entity.normal = normal;
    entity.width_factor = if width.is_finite() && width > 0.0 { width } else { 1.0 };
    entity.oblique_angle = if oblique.is_finite() { oblique } else { 0.0 };
    push(output, state, EntityType::Text(entity));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u32, payload: &[u8]) -> ProxyGraphicRecord {
        ProxyGraphicRecord::Unknown { record_type: kind, data: payload.to_vec() }
    }

    fn bytes(values: &[f64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn context() -> (Vec<String>, Vec<String>) {
        (vec!["0".into(), "C-TOPO".into()], vec!["ByBlock".into(), "ByLayer".into(), "Continuous".into(), "DASHED".into()])
    }

    #[test]
    fn primitives_and_traits() {
        let (layers, linetypes) = context();
        let ctx = ProxyDrawContext { layers: &layers, linetypes: &linetypes, layer: "SUP", unicode: false };
        let mut polyline = 3u32.to_le_bytes().to_vec();
        polyline.extend(bytes(&[0.0, 0.0, 0.0, 10.0, 0.0, 0.0, 10.0, 5.0, 0.0]));
        let mut circle = bytes(&[1.0, 2.0, 0.0, 3.0]);
        circle.extend(bytes(&[0.0, 0.0, 1.0]));
        let mut arc = bytes(&[0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, std::f64::consts::FRAC_PI_2]);
        arc.extend(0u32.to_le_bytes());
        let mut text = bytes(&[5.0, 5.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0, 2.5, 1.0, 0.0]);
        text.extend(b"P-12\0\0\0\0");
        let mut polygon = 3u32.to_le_bytes().to_vec();
        polygon.extend(bytes(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]));
        let graphics = ProxyGraphics {
            records: vec![
                record(1, &bytes(&[0.0; 6])),
                record(6, &polyline),
                record(14, &1u32.to_le_bytes()),
                record(16, &1u32.to_le_bytes()),
                record(18, &3u32.to_le_bytes()),
                record(2, &circle),
                record(4, &arc),
                record(22, &0xC2_0A_14_1Eu32.to_le_bytes()),
                record(10, &text),
                record(20, &1u32.to_le_bytes()),
                record(7, &polygon),
            ],
        };
        // Through the binary form: what a DWG carries.
        let graphics = ProxyGraphics::decode(&graphics.encode().unwrap()).unwrap();
        let e = graphics.to_entities(&ctx);
        assert_eq!(e.len(), 5);
        let EntityType::Polyline3D(p) = &e[0] else { panic!("{:?}", e[0]) };
        assert_eq!((p.vertices.len(), p.common.layer.as_str(), p.common.color), (3, "SUP", Color::ByBlock));
        let EntityType::Circle(c) = &e[1] else { panic!() };
        assert_eq!((c.center, c.radius, c.common.color, c.common.layer.as_str(), c.common.linetype.as_str()), (Vector3::new(1.0, 2.0, 0.0), 3.0, Color::Index(1), "C-TOPO", "DASHED"));
        let EntityType::Arc(a) = &e[2] else { panic!() };
        assert!((a.start_angle - std::f64::consts::FRAC_PI_2).abs() < 1e-12 && (a.end_angle - std::f64::consts::PI).abs() < 1e-12);
        let EntityType::Text(t) = &e[3] else { panic!() };
        assert_eq!((t.value.as_str(), t.height, t.common.color), ("P-12", 2.5, Color::Rgb { r: 0x0A, g: 0x14, b: 0x1E }));
        assert!((t.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!(matches!(&e[4], EntityType::Hatch(h) if h.paths.len() == 1));
    }

    #[test]
    fn shell_matrix_and_text2() {
        let (layers, linetypes) = context();
        let ctx = ProxyDrawContext { layers: &layers, linetypes: &linetypes, layer: "SUP", unicode: true };
        // Two triangles sharing an edge.
        let mut shell = 4u32.to_le_bytes().to_vec();
        shell.extend(bytes(&[0.0, 0.0, 1.0, 1.0, 0.0, 2.0, 1.0, 1.0, 3.0, 0.0, 1.0, 4.0]));
        shell.extend(8u32.to_le_bytes());
        for i in [3u32, 0, 1, 2, 3, 0, 2, 3] {
            shell.extend(i.to_le_bytes());
        }
        let matrix = bytes(&[1.0, 0.0, 0.0, 100.0, 0.0, 1.0, 0.0, 200.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        let mut text2 = bytes(&[1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0]);
        for u in "Eje 1\0".encode_utf16() {
            text2.extend(u.to_le_bytes());
        }
        text2.extend(5i32.to_le_bytes());
        text2.extend(0i32.to_le_bytes());
        text2.extend(bytes(&[0.8, 0.9, 0.0, 1.0]));
        let graphics = ProxyGraphics {
            records: vec![
                record(29, &matrix),
                record(9, &shell),
                record(31, &[]),
                record(38, &text2),
            ],
        };
        let e = graphics.to_entities(&ctx);
        assert_eq!(e.len(), 3, "{e:?}");
        let EntityType::Polyline3D(p) = &e[0] else { panic!() };
        assert!(p.flags.closed && p.vertices.len() == 3);
        assert_eq!(p.vertices[1].position, Vector3::new(101.0, 200.0, 2.0));
        let EntityType::Text(t) = &e[2] else { panic!() };
        assert_eq!((t.value.as_str(), t.height, t.width_factor, t.insertion_point), ("Eje 1", 0.8, 0.9, Vector3::new(1.0, 1.0, 0.0)));
    }

    #[test]
    fn truncated_records_are_skipped() {
        let (layers, linetypes) = context();
        let ctx = ProxyDrawContext { layers: &layers, linetypes: &linetypes, layer: "0", unicode: false };
        let graphics = ProxyGraphics {
            records: vec![record(6, &u32::MAX.to_le_bytes()), record(9, &[1, 0, 0, 0]), record(2, &[0; 10]), record(29, &[0; 8]), record(31, &[])],
        };
        assert!(graphics.to_entities(&ctx).is_empty());
    }
}
