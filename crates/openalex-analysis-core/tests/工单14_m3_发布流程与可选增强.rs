use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use cistella_core::{
    UpdateCheck, check_update, is_newer_version, read_latest_version_from_json,
    read_version_from_tauri_conf,
};

fn temp_dir(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{}-{}",
        prefix,
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn write_tauri_conf(dir: &Path, version: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("tauri.conf.json");
    fs::write(
        &path,
        format!(
            r#"{{"productName":"cistella","version":"{}","identifier":"org.cistella.desktop"}}"#,
            version
        ),
    )
    .unwrap();
    path
}

fn write_update_json(dir: &Path, latest_version: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join("update.json");
    fs::write(
        &path,
        format!(r#"{{"latest_version":"{}"}}"#, latest_version),
    )
    .unwrap();
    path
}

#[test]
fn current_version_is_parsed_from_tauri_conf() {
    let dir = temp_dir("cistella-m3-version");
    let conf = write_tauri_conf(&dir, "0.1.0");

    let version = read_version_from_tauri_conf(&conf).unwrap();
    assert_eq!(version, "0.1.0");

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn update_is_reported_when_latest_is_newer() {
    let dir = temp_dir("cistella-m3-update-available");
    let conf = write_tauri_conf(&dir, "0.1.0");
    let update = write_update_json(&dir, "0.2.0");

    let check = check_update(&conf, &update).unwrap();
    assert_eq!(
        check,
        UpdateCheck {
            current_version: "0.1.0".to_string(),
            latest_version: "0.2.0".to_string(),
            has_update: true,
        }
    );

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn no_update_is_reported_when_versions_are_equal() {
    let dir = temp_dir("cistella-m3-no-update");
    let conf = write_tauri_conf(&dir, "0.1.0");
    let update = write_update_json(&dir, "0.1.0");

    let check = check_update(&conf, &update).unwrap();
    assert!(!check.has_update);
    assert_eq!(check.current_version, "0.1.0");
    assert_eq!(check.latest_version, "0.1.0");

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn semver_comparison_handles_double_digit_minor_versions() {
    // Lexicographic comparison would incorrectly say "0.2.0" > "0.10.0".
    assert!(is_newer_version("0.2.0", "0.10.0").unwrap());
    assert!(!is_newer_version("0.10.0", "0.10.0").unwrap());
    assert!(!is_newer_version("0.11.0", "0.10.0").unwrap());
}

#[test]
fn missing_update_json_returns_error_instead_of_panicking() {
    let dir = temp_dir("cistella-m3-missing-update");
    let conf = write_tauri_conf(&dir, "0.1.0");
    let missing = dir.join("does-not-exist.json");

    let result = check_update(&conf, &missing);
    assert!(result.is_err());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn malformed_update_json_returns_error_instead_of_panicking() {
    let dir = temp_dir("cistella-m3-malformed-update");
    let conf = write_tauri_conf(&dir, "0.1.0");
    let update = dir.join("update.json");
    fs::create_dir_all(&dir).unwrap();
    fs::write(&update, b"not valid json").unwrap();

    let result = check_update(&conf, &update);
    assert!(result.is_err());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn update_json_missing_latest_version_returns_error() {
    let dir = temp_dir("cistella-m3-missing-latest");
    write_tauri_conf(&dir, "0.1.0");
    let update = dir.join("update.json");
    fs::create_dir_all(&dir).unwrap();
    fs::write(&update, br#"{"other_field":"value"}"#).unwrap();

    let result = read_latest_version_from_json(&update);
    assert!(result.is_err());

    fs::remove_dir_all(&dir).unwrap();
}
