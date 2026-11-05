use super::*;
use std::ffi::OsString;
use std::path::Path;

#[test]
fn windows_uses_explorer_with_a_quoted_path_and_no_cmd() {
    let (program, arg) = open_argv(Os::Windows, Path::new(r"C:\R&D\x^y\%PATH%\shot.png"));
    assert_eq!(program, "explorer.exe");
    assert_eq!(arg, OsString::from(r#""C:\R&D\x^y\%PATH%\shot.png""#));
}

#[test]
fn macos_uses_open_and_others_xdg_open() {
    let (program, arg) = open_argv(Os::MacOs, Path::new("/r/trace.zip"));
    assert_eq!(program, "open");
    assert_eq!(arg, OsString::from("/r/trace.zip"));
    let (program, _) = open_argv(Os::Other, Path::new("/r/trace.zip"));
    assert_eq!(program, "xdg-open");
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("guard-gui-open-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("e2e").join("test-results")).unwrap();
    dir
}

#[test]
fn an_attachment_inside_the_playwright_dir_with_a_viewer_extension_is_allowed() {
    let dir = scratch("ok");
    let root = dir.join("e2e");
    let shot = root.join("test-results").join("Shot.PNG");
    std::fs::write(&shot, b"x").unwrap();
    assert_eq!(checked(&root, &shot), Ok(shot.clone()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn executables_are_refused_even_inside_the_playwright_dir() {
    let dir = scratch("exe");
    let root = dir.join("e2e");
    for name in ["evil.exe", "evil.bat", "evil.cmd", "noext"] {
        let file = root.join("test-results").join(name);
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(checked(&root, &file), Err(OpenError::Extension), "{name}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_path_escaping_the_playwright_dir_is_refused() {
    let dir = scratch("out");
    let root = dir.join("e2e");
    let outside = dir.join("secret.png");
    std::fs::write(&outside, b"x").unwrap();
    let sneaky = root
        .join("test-results")
        .join("..")
        .join("..")
        .join("secret.png");
    assert!(matches!(
        checked(&root, &sneaky),
        Err(OpenError::Outside(_))
    ));
    assert!(matches!(
        checked(&root, &outside),
        Err(OpenError::Outside(_))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_file_is_an_error() {
    let dir = scratch("missing");
    let root = dir.join("e2e");
    let gone = root.join("test-results").join("gone.png");
    assert!(matches!(checked(&root, &gone), Err(OpenError::Io(_))));
    let _ = std::fs::remove_dir_all(&dir);
}
