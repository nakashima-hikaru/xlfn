#![cfg(feature = "serde")]

use xlfn::ExcelError;
use xlfn::value::{ExcelCellOutput, ExcelCellValue, ExcelValue, Matrix, OptionalExcelValue};

#[test]
fn owned_value_roundtrips_preserve_presence_errors_and_shape() {
    let cells = vec![
        ExcelCellValue::Number(3.5),
        ExcelCellValue::Boolean(true),
        ExcelCellValue::String("日本語".to_owned()),
        ExcelCellValue::Error(ExcelError::NotAvailable),
        ExcelCellValue::Blank,
        ExcelCellValue::String(String::new()),
    ];
    let value = ExcelValue::Array(Matrix::new(2, 3, cells).unwrap());
    let json = serde_json::to_string(&value).unwrap();
    assert_eq!(serde_json::from_str::<ExcelValue>(&json).unwrap(), value);
    for value in [
        OptionalExcelValue::Missing,
        OptionalExcelValue::Blank,
        OptionalExcelValue::Value(value),
    ] {
        let json = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<OptionalExcelValue<ExcelValue>>(&json).unwrap(),
            value
        );
    }
    let output = ExcelCellOutput::Error(ExcelError::Value);
    assert_eq!(
        serde_json::from_str::<ExcelCellOutput>(&serde_json::to_string(&output).unwrap()).unwrap(),
        output
    );
}

#[test]
fn serialization_rejects_nonfinite_cells_and_deserialization_rejects_bad_arrays() {
    for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(serde_json::to_string(&ExcelCellValue::Number(number)).is_err());
        assert!(serde_json::to_string(&ExcelCellOutput::Number(number)).is_err());
    }
    assert!(serde_json::from_str::<ExcelCellValue>(r#"{"Number":1e999}"#).is_err());
    assert!(
        serde_json::from_str::<ExcelValue>(r#"{"Array":{"rows":2,"columns":2,"data":["Blank"]}}"#)
            .is_err()
    );
}
