use super::*;

#[test]
fn coded_font_name_helpers() {
    assert_eq!(coded_font_point_size("C0H200B0"), Some(12.0));
    assert_eq!(coded_font_point_size("C0H20000"), Some(10.0));
    assert_eq!(coded_font_point_size("X0H200B0"), None);
    assert_eq!(coded_font_substitute("C04200B0"), Some(("Courier New, monospace", FontWeight::Normal)));
    assert_eq!(substitute_font("TIMES-BOLD"), Some(("Times New Roman, serif", FontWeight::Bold)));
    assert_eq!(py_title("TIMES-ROMAN").replace('-', " "), "Times Roman");
    assert_eq!(rotation_insensitive("X1ARBF").as_deref(), Some("XARBF"));
}

#[test]
fn scale_floors_like_python() {
    assert_eq!(scale(-1, 3, 1), -1);
    assert_eq!(scale(10, 0, 5), 10);
    assert_eq!(scale(300, 300, 1440), 1440);
}
