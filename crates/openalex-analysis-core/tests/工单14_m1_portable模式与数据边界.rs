use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use cistella_core::{
    AppConfig, AppDirectories, RecentVault, load_app_config, load_recent_vaults,
    make_vault_paths_portable, migrate_vaults_to_installed, migrate_vaults_to_portable,
    resolve_recent_vault_paths, save_app_config, save_recent_vaults,
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

fn write_test_vault(root: &Path) {
    fs::create_dir_all(root.join("tables")).unwrap();
    fs::write(root.join("manifest.json"), r#"{"format_version":"0.1.0","vault_id":"m1-test","logical_schema_version":"0.1.0","created_at":"2026-08-30T00:00:00Z","source":{"name":"Test","entity":"sources","snapshot_date":null,"input_path":"input"},"tables":{}}"#).unwrap();
    fs::write(root.join("tables").join("sources.parquet"), b"dummy").unwrap();
}

#[test]
fn portable_mode_is_detected_when_marker_directory_exists() {
    let exe_dir = temp_dir("cistella-m1-portable-detect");
    fs::create_dir_all(&exe_dir).unwrap();
    fs::create_dir_all(exe_dir.join("cistella-portable")).unwrap();

    let dirs = AppDirectories::from_exe_dir(&exe_dir).unwrap();
    assert!(dirs.is_portable_mode());
    assert_eq!(
        dirs.portable_root(),
        Some(exe_dir.join("cistella-portable").as_path())
    );
    assert!(
        dirs.config_dir()
            .starts_with(exe_dir.join("cistella-portable"))
    );

    fs::remove_dir_all(&exe_dir).unwrap();
}

#[test]
fn installed_mode_is_detected_when_marker_directory_missing() {
    let exe_dir = temp_dir("cistella-m1-installed-detect");
    fs::create_dir_all(&exe_dir).unwrap();

    let dirs = AppDirectories::from_exe_dir(&exe_dir).unwrap();
    assert!(!dirs.is_portable_mode());
    assert!(dirs.portable_root().is_none());

    fs::remove_dir_all(&exe_dir).unwrap();
}

#[test]
fn recent_vaults_roundtrip_is_consistent() {
    let data_dir = temp_dir("cistella-m1-recent-roundtrip");
    let dirs = AppDirectories::from_test_dirs(None, Some(data_dir.clone())).unwrap();
    let vaults = vec![RecentVault {
        path: "/tmp/my-vault".to_string(),
        name: "My Vault".to_string(),
        opened_at: Some("2026-08-30T12:00:00Z".to_string()),
    }];

    save_recent_vaults(dirs.recent_vaults_path(), &vaults).unwrap();
    let loaded = load_recent_vaults(dirs.recent_vaults_path()).unwrap();
    assert_eq!(loaded, vaults);

    fs::remove_dir_all(&data_dir).unwrap();
}

#[test]
fn portable_mode_stores_relative_paths_and_resolves_to_absolute() {
    let portable_root = temp_dir("cistella-m1-relative-paths");
    let dirs = AppDirectories::from_test_dirs(Some(portable_root.clone()), None).unwrap();
    let vault_inside = portable_root.join("vaults").join("my-library");
    fs::create_dir_all(&vault_inside).unwrap();
    write_test_vault(&vault_inside);

    let vaults = vec![RecentVault {
        path: vault_inside.to_string_lossy().to_string(),
        name: "My Library".to_string(),
        opened_at: None,
    }];
    // In portable mode we must relativize vault paths under the portable root
    // before saving, so the persisted JSON remains valid after the bundle is
    // moved to another location.
    let portable_vaults = make_vault_paths_portable(&portable_root, &vaults);
    save_recent_vaults(dirs.recent_vaults_path(), &portable_vaults).unwrap();

    // The persisted JSON must contain a relative path, not the absolute one.
    let raw = fs::read_to_string(dirs.recent_vaults_path()).unwrap();
    let stored: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        stored["vaults"][0]["path"].as_str(),
        Some("vaults/my-library")
    );
    assert!(!raw.contains(&vault_inside.to_string_lossy().to_string()));

    // Loading resolves the relative path back to an absolute path.
    let loaded = load_recent_vaults(dirs.recent_vaults_path()).unwrap();
    let resolved = resolve_recent_vault_paths(&portable_root, &loaded);
    assert_eq!(PathBuf::from(&resolved[0].path), vault_inside);

    fs::remove_dir_all(&portable_root).unwrap();
}

#[test]
fn app_directories_save_recent_vaults_relativizes_inside_portable_root() {
    let portable_root = temp_dir("cistella-m1-dirs-save-relative");
    let dirs = AppDirectories::from_test_dirs(Some(portable_root.clone()), None).unwrap();
    let vault_inside = portable_root.join("vaults").join("my-library");
    fs::create_dir_all(&vault_inside).unwrap();
    write_test_vault(&vault_inside);

    let vaults = vec![RecentVault {
        path: vault_inside.to_string_lossy().to_string(),
        name: "My Library".to_string(),
        opened_at: None,
    }];
    dirs.save_recent_vaults(&vaults).unwrap();

    let raw = fs::read_to_string(dirs.recent_vaults_path()).unwrap();
    let stored: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        stored["vaults"][0]["path"].as_str(),
        Some("vaults/my-library")
    );

    let loaded = load_recent_vaults(dirs.recent_vaults_path()).unwrap();
    let resolved = resolve_recent_vault_paths(&portable_root, &loaded);
    assert_eq!(PathBuf::from(&resolved[0].path), vault_inside);

    fs::remove_dir_all(&portable_root).unwrap();
}

#[test]
fn app_config_roundtrip_is_consistent() {
    let data_dir = temp_dir("cistella-m1-config-roundtrip");
    let dirs = AppDirectories::from_test_dirs(None, Some(data_dir.clone())).unwrap();
    let config = AppConfig {
        theme: Some("dark".to_string()),
        window_size: Some(cistella_core::WindowSize {
            width: 1280,
            height: 720,
        }),
        ..Default::default()
    };

    save_app_config(dirs.app_config_path(), &config).unwrap();
    let loaded = load_app_config(dirs.app_config_path()).unwrap();
    assert_eq!(loaded, config);

    fs::remove_dir_all(&data_dir).unwrap();
}

#[test]
fn migrate_to_portable_copies_vault_and_returns_relative_paths() {
    let source_vault = temp_dir("cistella-m1-migrate-source");
    write_test_vault(&source_vault);
    let portable_root = temp_dir("cistella-m1-migrate-portable");
    let dirs = AppDirectories::from_test_dirs(Some(portable_root.clone()), None).unwrap();

    let vaults = vec![RecentVault {
        path: source_vault.to_string_lossy().to_string(),
        name: "Source Vault".to_string(),
        opened_at: None,
    }];
    let migrated = migrate_vaults_to_portable(&dirs, &vaults).unwrap();

    // Returned path is relative to portable root.
    assert_eq!(migrated[0].path, "vaults/Source Vault");

    // Vault was copied under portable root.
    let copied = portable_root.join("vaults").join("Source Vault");
    assert!(copied.join("manifest.json").exists());
    assert!(copied.join("tables").join("sources.parquet").exists());

    // Original vault is preserved.
    assert!(source_vault.join("manifest.json").exists());

    fs::remove_dir_all(&source_vault).unwrap();
    fs::remove_dir_all(&portable_root).unwrap();
}

#[test]
fn migrate_failure_does_not_destroy_source_vault() {
    // Use a file (not a directory) as the source. It exists, so the existence
    // check passes, but copying a directory tree from a file fails without
    // mutating the source.
    let source_vault = temp_dir("cistella-m1-migrate-fail-source").join("not-a-dir.txt");
    fs::create_dir_all(source_vault.parent().unwrap()).unwrap();
    fs::write(&source_vault, b"vault content").unwrap();

    let portable_root = temp_dir("cistella-m1-migrate-fail-portable");
    let dirs = AppDirectories::from_test_dirs(Some(portable_root.clone()), None).unwrap();

    let vaults = vec![RecentVault {
        path: source_vault.to_string_lossy().to_string(),
        name: "Protected Vault".to_string(),
        opened_at: None,
    }];
    let result = migrate_vaults_to_portable(&dirs, &vaults);
    assert!(result.is_err());

    // Source file remains intact.
    assert!(source_vault.exists());
    assert_eq!(fs::read_to_string(&source_vault).unwrap(), "vault content");

    fs::remove_dir_all(source_vault.parent().unwrap()).unwrap();
    fs::remove_dir_all(&portable_root).unwrap();
}

#[test]
fn portable_mode_does_not_write_to_system_data_dir() {
    let exe_dir = temp_dir("cistella-m1-portable-isolation");
    fs::create_dir_all(&exe_dir).unwrap();
    fs::create_dir_all(exe_dir.join("cistella-portable")).unwrap();

    let dirs = AppDirectories::from_exe_dir(&exe_dir).unwrap();
    assert!(dirs.is_portable_mode());

    let config = AppConfig::default();
    save_app_config(dirs.app_config_path(), &config).unwrap();
    save_recent_vaults(dirs.recent_vaults_path(), &[]).unwrap();

    // All writes must be under the portable root.
    assert!(
        dirs.app_config_path()
            .starts_with(exe_dir.join("cistella-portable"))
    );
    assert!(
        dirs.recent_vaults_path()
            .starts_with(exe_dir.join("cistella-portable"))
    );
    assert!(
        dirs.config_dir()
            .starts_with(exe_dir.join("cistella-portable"))
    );

    // installed_data_dir is not set in portable mode.
    assert!(dirs.portable_root().is_some());

    fs::remove_dir_all(&exe_dir).unwrap();
}

#[test]
fn migrate_to_installed_copies_vault_and_returns_absolute_paths() {
    let portable_root = temp_dir("cistella-m1-migrate-to-installed-portable");
    let source_vault = portable_root.join("vaults").join("My_Library");
    fs::create_dir_all(&source_vault).unwrap();
    write_test_vault(&source_vault);
    let installed_dir = temp_dir("cistella-m1-migrate-to-installed-data");
    let dirs = AppDirectories::from_test_dirs(None, Some(installed_dir.clone())).unwrap();

    // Installed mode receives absolute paths (the UI resolves portable-relative
    // paths before calling migration).
    let vaults = vec![RecentVault {
        path: source_vault.to_string_lossy().to_string(),
        name: "My Library".to_string(),
        opened_at: None,
    }];
    let migrated = migrate_vaults_to_installed(&dirs, &vaults).unwrap();

    // Returned path is absolute.
    let target = installed_dir.join("vaults").join("My Library");
    assert_eq!(migrated[0].path, target.to_string_lossy().to_string());
    assert!(target.join("manifest.json").exists());

    fs::remove_dir_all(&portable_root).unwrap();
    fs::remove_dir_all(&installed_dir).unwrap();
}
