use std::io::Cursor;

use acadrust::entities::{EntityType, Text};
use acadrust::io::dwg::dwg_stream_readers::bit_reader::DwgBitReader;
use acadrust::io::dwg::dwg_stream_writers::bit_writer::DwgBitWriter;
use acadrust::io::dwg::DwgVersion;
use acadrust::io::dxf::code_page::{
    decode_legacy_escapes, dwg_code_page_index, encode_legacy_string, encoding_from_code_page,
    encoding_from_dwg_code_page,
};
use acadrust::objects::{ObjectType, XRecord};
use acadrust::tables::{AppId, Layer};
use acadrust::types::{DxfVersion, Vector3};
use acadrust::xdata::{ExtendedDataRecord, XDataValue};
use acadrust::{CadDocument, DwgReader, DwgWriter, DxfReader, DxfWriter};

const SAMPLES: &[(&str, &[u8], &str)] = &[
    ("DOS437", &[0x82], "\u{E9}"),
    ("DOS850", &[0xD5], "\u{131}"),
    ("DOS852", &[0xA5], "\u{105}"),
    ("DOS855", &[0x80], "\u{452}"),
    ("DOS857", &[0x8D], "\u{131}"),
    ("DOS860", &[0x84], "\u{E3}"),
    ("DOS861", &[0x8C], "\u{F0}"),
    ("DOS863", &[0x84], "\u{C2}"),
    ("DOS864", &[0xB0], "\u{660}"),
    ("DOS865", &[0x9B], "\u{F8}"),
    ("DOS869", &[0xA4], "\u{391}"),
    ("DOS866", &[0x80], "\u{410}"),
    ("MAC-ROMAN", &[0x8E], "\u{E9}"),
    ("JOHAB", &[0x88, 0x61], "\u{AC00}"),
];

fn mappings(source: &str) -> impl Iterator<Item = (u16, char)> + '_ {
    source.lines().filter_map(|line| {
        let mut fields = line.split('#').next()?.split_whitespace();
        let code = u16::from_str_radix(fields.next()?.strip_prefix("0x")?, 16).ok()?;
        let scalar = u32::from_str_radix(fields.next()?.strip_prefix("0x")?, 16).ok()?;
        Some((code, char::from_u32(scalar)?))
    })
}

#[test]
fn every_defined_oem_byte_matches_authoritative_mapping_in_both_directions() {
    for (page, mapping) in [
        ("DOS437", include_str!("fixtures/code_pages/CP437.TXT")),
        ("DOS850", include_str!("fixtures/code_pages/CP850.TXT")),
        ("DOS852", include_str!("fixtures/code_pages/CP852.TXT")),
        ("DOS855", include_str!("fixtures/code_pages/CP855.TXT")),
        ("DOS857", include_str!("fixtures/code_pages/CP857.TXT")),
        ("DOS860", include_str!("fixtures/code_pages/CP860.TXT")),
        ("DOS861", include_str!("fixtures/code_pages/CP861.TXT")),
        ("DOS863", include_str!("fixtures/code_pages/CP863.TXT")),
        ("DOS864", include_str!("fixtures/code_pages/CP864.TXT")),
        ("DOS865", include_str!("fixtures/code_pages/CP865.TXT")),
        ("DOS869", include_str!("fixtures/code_pages/CP869.TXT")),
    ] {
        let encoding = encoding_from_code_page(page).unwrap();
        let mut defined = [false; 256];
        for (code, ch) in mappings(mapping) {
            let bytes = [code as u8];
            let expected = ch.to_string();
            let (decoded, _, errors) = encoding.decode(&bytes);
            assert!(!errors, "{page} {code:02X}");
            assert_eq!(decoded, expected, "{page} {code:02X}");
            assert_eq!(
                encode_legacy_string(&expected, encoding),
                bytes,
                "{page} {code:02X}"
            );
            defined[code as usize] = true;
        }
        for (code, defined) in defined.into_iter().enumerate() {
            if !defined {
                assert!(
                    encoding.decode(&[code as u8]).2,
                    "undefined {page} {code:02X}"
                );
            }
        }
    }
}

#[test]
fn every_johab_hangul_jamo_symbol_and_hanja_matches_authoritative_mapping() {
    let encoding = encoding_from_code_page("JOHAB").unwrap();
    let mut count = 0;
    for (code, mut ch) in mappings(include_str!("fixtures/code_pages/JOHAB.TXT")) {
        // CP1361 uses US-ASCII, preserving CAD paths and CIF/MIF backslashes.
        if code == 0x5C {
            ch = '\\';
        }
        let packed = code.to_be_bytes();
        let bytes = if code < 128 {
            &packed[1..]
        } else {
            &packed[..]
        };
        let expected = ch.to_string();
        assert_eq!(encoding.decode(bytes).0, expected, "JOHAB {code:04X}");
        assert!(!encoding.decode(bytes).2, "JOHAB {code:04X}");
        assert_eq!(
            encode_legacy_string(&expected, encoding),
            bytes,
            "JOHAB {code:04X}"
        );
        count += 1;
    }
    assert!(count > 17000, "the complete mapping must be exercised");
    assert_eq!(encode_legacy_string("\u{20AC}", encoding), [0xD9, 0xE6]);
    assert_eq!(encoding.decode(&[0xD9, 0xE6]).0, "\u{20AC}");
    for bytes in [
        &[0x88][..],
        &[0x88, 0x00],
        &[0x84, 0x41],
        &[0xDA, 0xA1],
        &[0xFF, 0xFF],
    ] {
        assert!(encoding.decode(bytes).2, "invalid JOHAB {bytes:02X?}");
    }
    assert_eq!(encoding.decode(&[0x88, 0x00, b'A']).0, "\u{FFFD}\0A");
}

#[test]
fn mac_roman_high_bytes_match_authoritative_mapping() {
    let encoding = encoding_from_code_page("MAC-ROMAN").unwrap();
    for (code, ch) in mappings(include_str!("fixtures/code_pages/ROMAN.TXT")) {
        if code < 128 {
            continue;
        }
        assert_eq!(
            encoding.decode(&[code as u8]).0,
            ch.to_string(),
            "MAC-ROMAN {code:02X}"
        );
        assert_eq!(
            encode_legacy_string(&ch.to_string(), encoding),
            [code as u8]
        );
    }
}

fn raw_dxf(page: &str, raw: &[u8], version: DxfVersion, binary: bool) -> Vec<u8> {
    if binary {
        let mut bytes = b"AutoCAD Binary DXF\r\n\x1A\0".to_vec();
        let mut string = |code: i16, value: &[u8]| {
            bytes.extend_from_slice(&code.to_le_bytes());
            bytes.extend_from_slice(value);
            bytes.push(0);
        };
        for (code, value) in [
            (0, "SECTION"),
            (2, "HEADER"),
            (9, "$ACADVER"),
            (1, version.to_dxf_string()),
            (9, "$DWGCODEPAGE"),
            (3, page),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "TEXT"),
            (5, "100"),
            (8, "0"),
        ] {
            string(code, value.as_bytes());
        }
        string(1, raw);
        string(0, b"ENDSEC");
        string(0, b"EOF");
        bytes
    } else {
        let mut bytes = format!(
            "0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\n{}\n9\n$DWGCODEPAGE\n3\n{page}\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nTEXT\n5\n100\n8\n0\n10\n0\n20\n0\n40\n1\n1\n",
            version.to_dxf_string()
        ).into_bytes();
        bytes.extend_from_slice(raw);
        bytes.extend_from_slice(b"\n0\nENDSEC\n0\nEOF\n");
        bytes
    }
}

fn text_value(document: &CadDocument) -> String {
    document
        .entities()
        .find_map(|entity| match entity {
            EntityType::Text(text) => Some(text.value.clone()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn external_legacy_dxf_bytes_decode_in_ascii_and_binary_for_every_page() {
    for &(page, bytes, expected) in SAMPLES {
        for version in [
            DxfVersion::AC1012,
            DxfVersion::AC1014,
            DxfVersion::AC1015,
            DxfVersion::AC1018,
        ] {
            for binary in [false, true] {
                let document =
                    DxfReader::from_reader(Cursor::new(raw_dxf(page, bytes, version, binary)))
                        .unwrap()
                        .read()
                        .unwrap();
                assert_eq!(
                    text_value(&document),
                    expected,
                    "{page} {version:?} binary={binary}"
                );
            }
        }
    }
    // Valid UTF-8 bytes can also be an unrelated pair of OEM characters.
    for binary in [false, true] {
        let document = DxfReader::from_reader(Cursor::new(raw_dxf(
            "DOS850",
            &[0xC3, 0xA9],
            DxfVersion::AC1015,
            binary,
        )))
        .unwrap()
        .read()
        .unwrap();
        assert_eq!(text_value(&document), "\u{251C}\u{AE}");
    }
}

#[test]
fn dxf_page_before_version_is_honored_and_modern_versions_override_default_encoding() {
    let mut bytes = b"0\nSECTION\n2\nHEADER\n9\n$DWGCODEPAGE\n3\nDOS437\n9\n$ACADVER\n1\nAC1015\n0\nENDSEC\n0\nSECTION\n2\nENTITIES\n0\nTEXT\n5\n100\n8\n0\n1\n".to_vec();
    bytes.extend_from_slice(&[0x82]);
    bytes.extend_from_slice(b"\n0\nENDSEC\n0\nEOF\n");
    let read = DxfReader::from_reader(Cursor::new(bytes))
        .unwrap()
        .read()
        .unwrap();
    assert_eq!(text_value(&read), "\u{E9}");
    for binary in [false, true] {
        let bytes = raw_dxf("DOS437", "\u{E9}".as_bytes(), DxfVersion::AC1021, binary);
        let read = DxfReader::from_reader(Cursor::new(bytes))
            .unwrap()
            .read()
            .unwrap();
        assert_eq!(text_value(&read), "\u{E9}");
    }
}

#[test]
fn dxf_output_contains_declared_external_bytes_and_preserves_unmappable_text() {
    for &(page, bytes, expected) in SAMPLES {
        for version in [
            DxfVersion::AC1012,
            DxfVersion::AC1014,
            DxfVersion::AC1015,
            DxfVersion::AC1018,
        ] {
            let mut document = CadDocument::with_version(version);
            document.header.code_page = page.into();
            let value = format!("X{expected}\u{1F600}");
            document
                .add_entity(EntityType::Text(Text::with_value(&value, Vector3::ZERO)))
                .unwrap();
            for binary in [false, true] {
                let output = if binary {
                    DxfWriter::new_binary(&document)
                } else {
                    DxfWriter::new(&document)
                }
                .write_to_vec()
                .unwrap();
                let encoded = if binary && version < DxfVersion::AC1021 {
                    // Binary legacy streams are promoted to R2010 for
                    // BricsCAD compatibility, so strings use UTF-8 directly.
                    format!("X{expected}\u{1F600}").into_bytes()
                } else {
                    let mut encoded = b"X".to_vec();
                    encoded.extend_from_slice(bytes);
                    encoded.extend_from_slice(b"\\U+D83D\\U+DE00");
                    encoded
                };
                assert!(
                    output
                        .windows(encoded.len())
                        .any(|window| window == encoded),
                    "{page} {version:?} binary={binary}"
                );
                let read = DxfReader::from_reader(Cursor::new(output))
                    .unwrap()
                    .read()
                    .unwrap();
                assert_eq!(
                    text_value(&read),
                    value,
                    "{page} {version:?} binary={binary}"
                );
            }
        }
    }
}

#[test]
fn dwg_streams_use_exact_declared_bytes_and_files_preserve_all_supported_versions() {
    for &(page, bytes, expected) in SAMPLES {
        let encoding = encoding_from_dwg_code_page(dwg_code_page_index(page));
        for version in [
            DxfVersion::AC1009,
            DxfVersion::AC1012,
            DxfVersion::AC1014,
            DxfVersion::AC1015,
            DxfVersion::AC1018,
        ] {
            let dwg_version = DwgVersion::from_dxf_version(version).unwrap();
            let writer = DwgBitWriter::with_encoding(dwg_version, version, encoding);
            let reader = DwgBitReader::with_encoding(Vec::new(), dwg_version, version, encoding);
            assert_eq!(
                writer.encode_legacy_text(expected),
                bytes,
                "{page} {version:?}"
            );
            assert_eq!(
                reader.decode_legacy_text(bytes),
                expected,
                "{page} {version:?}"
            );
            let mut document = CadDocument::with_version(version);
            document.header.code_page = page.into();
            document.header.project_name = expected.into();
            document.header.menu_name = expected.into();
            let value = format!("X{expected}\u{1F600}");
            document
                .add_entity(EntityType::Text(Text::with_value(&value, Vector3::ZERO)))
                .unwrap();
            let output = DwgWriter::write_to_vec(&document).unwrap();
            let read = DwgReader::from_stream(Cursor::new(output)).read().unwrap();
            assert_eq!(read.header.code_page, page, "{version:?}");
            assert_eq!(text_value(&read), value, "{page} {version:?}");
            assert_eq!(read.header.menu_name, expected, "{page} {version:?}");
            if version >= DxfVersion::AC1018 {
                assert_eq!(read.header.project_name, expected, "{page} {version:?}");
            }
        }
    }
}

#[test]
fn legacy_table_names_eed_xrecord_and_summary_strings_use_selected_page() {
    for &(page, _, expected) in SAMPLES {
        for version in [
            DxfVersion::AC1012,
            DxfVersion::AC1014,
            DxfVersion::AC1015,
            DxfVersion::AC1018,
        ] {
            let mut document = CadDocument::with_version(version);
            document.header.code_page = page.into();
            document.summary_info.title = expected.into();
            document
                .summary_info
                .custom_properties
                .push(("name".into(), expected.into()));
            let layer = format!("L{expected}");
            document.layers.add(Layer::new(&layer)).unwrap();
            document.app_ids.add(AppId::new("ENCODING131")).unwrap();
            let value = format!("X{expected}\u{1F600}");
            let mut text = Text::with_value(&value, Vector3::ZERO);
            text.common.layer = layer.clone();
            let mut eed = ExtendedDataRecord::new("ENCODING131");
            eed.add_value(XDataValue::String(value.clone()));
            text.common.extended_data.add_record(eed);
            let text_handle = document.add_entity(EntityType::Text(text)).unwrap();
            let record_handle = document.allocate_handle();
            let mut record = XRecord::named("ENCODING131");
            record.handle = record_handle;
            record.owner = document.header.named_objects_dict_handle;
            record.add_string(1, value.clone());
            document
                .objects
                .insert(record_handle, ObjectType::XRecord(Box::new(record)));
            let output = DwgWriter::write_to_vec(&document).unwrap();
            let read = DwgReader::from_stream(Cursor::new(output)).read().unwrap();
            let text = read.get_entity(text_handle).unwrap();
            assert_eq!(text.common().layer, layer, "{page} {version:?}");
            assert!(read.layers.get(&layer).is_some(), "{page} {version:?}");
            assert_eq!(
                text.common()
                    .extended_data
                    .get_record("ENCODING131")
                    .unwrap()
                    .values,
                [XDataValue::String(value.clone())],
                "{page} {version:?}"
            );
            let ObjectType::XRecord(record) = read.objects.get(&record_handle).unwrap() else {
                panic!("XRECORD expected");
            };
            assert_eq!(
                record.get_first_by_code(1).unwrap().value.as_string(),
                Some(value.as_str()),
                "{page} {version:?}"
            );
            if version >= DxfVersion::AC1018 {
                assert_eq!(read.summary_info.title, expected, "{page} {version:?}");
                assert_eq!(
                    read.summary_info.custom_properties,
                    [("name".into(), expected.into())],
                    "{page} {version:?}"
                );
            }
        }
    }
}

#[test]
fn unicode_versions_ignore_legacy_page_and_johab_mif_escapes_work() {
    for version in [
        DxfVersion::AC1021,
        DxfVersion::AC1024,
        DxfVersion::AC1027,
        DxfVersion::AC1032,
    ] {
        let mut document = CadDocument::with_version(version);
        document.header.code_page = "DOS437".into();
        let value = "\u{E9}\u{AC00}\u{1F600}";
        document
            .add_entity(EntityType::Text(Text::with_value(value, Vector3::ZERO)))
            .unwrap();
        for binary in [false, true] {
            let output = if binary {
                DxfWriter::new_binary(&document)
            } else {
                DxfWriter::new(&document)
            }
            .write_to_vec()
            .unwrap();
            assert!(output
                .windows(value.len())
                .any(|window| window == value.as_bytes()));
            let read = DxfReader::from_reader(Cursor::new(output))
                .unwrap()
                .read()
                .unwrap();
            assert_eq!(text_value(&read), value);
        }
        let read = DwgReader::from_stream(Cursor::new(DwgWriter::write_to_vec(&document).unwrap()))
            .read()
            .unwrap();
        assert_eq!(text_value(&read), value);
    }
    assert_eq!(
        decode_legacy_escapes("\\M+48861\\M+4E031\\U+0021"),
        "\u{AC00}\u{4F3D}!"
    );
    assert_eq!(decode_legacy_escapes("\\M+48800"), "\\M+48800");
    assert_eq!(
        encode_legacy_string("\u{41F}\u{440}", encoding_from_code_page("DOS437").unwrap()),
        b"\\U+041F\\U+0440"
    );
}
