//! lince (DWG-04): the defined height of an MTEXT (DXF 46) has its own field
//! only from R2007. AutoCAD's SAVEAS 2000/2004 keeps it in `ACAD` XDATA:
//! "ACAD_MTEXT_DEFINED_HEIGHT_BEGIN", (1070 . 46), (1040 . h), "..._END".
//! Without it the height of a column MTEXT was lost (`Mechanical - Text and
//! Tables`: Σbbox −988 in fase0).

use crate::xdata::{ExtendedData, ExtendedDataRecord, XDataValue};

const BEGIN: &str = "ACAD_MTEXT_DEFINED_HEIGHT_BEGIN";
const END: &str = "ACAD_MTEXT_DEFINED_HEIGHT_END";

/// Takes the defined height out of the `ACAD` XDATA read from a DWG.
pub(crate) fn take(x: &mut ExtendedData) -> Option<f64> {
    let rec = x.records_mut().iter_mut().find(|r| r.application_name == "ACAD")?;
    let i = rec.values.iter().position(|v| matches!(v, XDataValue::String(s) if s == BEGIN))?;
    let h = match rec.values.get(i..i + 4)? {
        [_, XDataValue::Integer16(46), XDataValue::Real(h), XDataValue::String(end)] if end == END => *h,
        _ => return None,
    };
    rec.values.drain(i..i + 4);
    if rec.values.is_empty() {
        x.remove_record("ACAD");
    }
    Some(h)
}

/// Adds the defined height to the `ACAD` XDATA for a DWG before R2007.
pub(crate) fn add(x: &mut ExtendedData, h: f64) {
    let values = [XDataValue::String(BEGIN.into()), XDataValue::Integer16(46), XDataValue::Real(h), XDataValue::String(END.into())];
    match x.records_mut().iter_mut().find(|r| r.application_name == "ACAD") {
        Some(rec) if rec.values.iter().any(|v| matches!(v, XDataValue::String(s) if s == BEGIN)) => {}
        Some(rec) => rec.values.extend(values),
        None => {
            let mut rec = ExtendedDataRecord::new("ACAD");
            rec.values = values.to_vec();
            x.add_record(rec);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defined_height_goes_after_the_column_data_and_comes_back() {
        let mut x = ExtendedData::new();
        let mut rec = ExtendedDataRecord::new("ACAD");
        rec.values = vec![XDataValue::String("ACAD_MTEXT_COLUMNS_BEGIN".into()), XDataValue::String("ACAD_MTEXT_COLUMNS_END".into())];
        x.add_record(rec);
        add(&mut x, 500.0);
        add(&mut x, 500.0);
        assert_eq!(x.get_record("ACAD").unwrap().values.len(), 6);
        assert_eq!(take(&mut x), Some(500.0));
        assert_eq!(x.get_record("ACAD").unwrap().values.len(), 2);
        assert_eq!(take(&mut x), None);
        let mut solo = ExtendedData::new();
        add(&mut solo, 7.5);
        assert_eq!(take(&mut solo), Some(7.5));
        assert!(solo.is_empty());
    }
}
