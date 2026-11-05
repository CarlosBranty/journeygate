use super::*;
use std::path::Path;

/// 2026-11-05T10:15:00Z.
const STAMP_SECS: u64 = 1_793_873_700;

#[test]
fn the_stamp_is_utc_year_month_day_dash_hour_minute_second() {
    assert_eq!(file_stamp(STAMP_SECS), "20261105-101500");
    assert_eq!(file_stamp(0), "19700101-000000");
}

#[test]
fn each_format_has_its_cli_name_and_extension() {
    assert_eq!(ExportFormat::Markdown.cli_name(), "md");
    assert_eq!(ExportFormat::Markdown.extension(), "md");
    assert_eq!(ExportFormat::Excel.cli_name(), "xlsx");
    assert_eq!(ExportFormat::Excel.extension(), "xlsx");
}

#[test]
fn exports_land_in_the_project_guard_explain_folder() {
    let repo = Path::new("C:/work/shop");
    assert_eq!(
        export_path(repo, ExportFormat::Markdown, STAMP_SECS),
        repo.join(".guard")
            .join("explain")
            .join("explain-20261105-101500.md")
    );
    assert_eq!(
        export_path(repo, ExportFormat::Excel, STAMP_SECS),
        repo.join(".guard")
            .join("explain")
            .join("explain-20261105-101500.xlsx")
    );
}
