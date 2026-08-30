use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use cistella_core::{
    AppDirectories, RecentVault, backup_vault, migrate_vaults_to_installed,
    migrate_vaults_to_portable, restore_vault,
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
    fs::write(
        root.join("manifest.json"),
        r#"{"format_version":"0.1.0","vault_id":"m2-test","logical_schema_version":"0.1.0","created_at":"2026-08-30T00:00:00Z","source":{"name":"Test","entity":"sources","snapshot_date":null,"input_path":"input"},"tables":{}}"#,
    )
    .unwrap();
    fs::write(root.join("tables").join("sources.parquet"), b"vault data").unwrap();
    fs::create_dir_all(root.join("user")).unwrap();
    fs::write(
        root.join("user").join("literature_items.json"),
        b"[{\"itemId\":\"test\"}]",
    )
    .unwrap();
}

#[test]
fn backup_vault_to_zip_preserves_directory_structure() {
    let vault = temp_dir("cistella-m2-backup-source");
    write_test_vault(&vault);
    let backup = temp_dir("cistella-m2-backup-file").join("vault.zip");
    let restored = temp_dir("cistella-m2-backup-restored");

    backup_vault(&vault, &backup).unwrap();
    assert!(backup.exists());

    restore_vault(&backup, &restored).unwrap();
    assert!(restored.join("manifest.json").exists());
    assert_eq!(
        fs::read_to_string(restored.join("manifest.json")).unwrap(),
        fs::read_to_string(vault.join("manifest.json")).unwrap()
    );
    assert_eq!(
        fs::read(restored.join("tables").join("sources.parquet")).unwrap(),
        b"vault data"
    );
    assert_eq!(
        fs::read_to_string(restored.join("user").join("literature_items.json")).unwrap(),
        "[{\"itemId\":\"test\"}]"
    );

    fs::remove_dir_all(&vault).unwrap();
    fs::remove_dir_all(backup.parent().unwrap()).unwrap();
    fs::remove_dir_all(&restored).unwrap();
}

#[test]
fn restore_vault_fails_when_target_already_exists() {
    let vault = temp_dir("cistella-m2-restore-existing-source");
    write_test_vault(&vault);
    let backup = temp_dir("cistella-m2-restore-existing-file").join("vault.zip");
    let existing = temp_dir("cistella-m2-restore-existing-target");
    fs::create_dir_all(&existing).unwrap();
    fs::write(existing.join("do-not-overwrite.txt"), b"original").unwrap();

    backup_vault(&vault, &backup).unwrap();
    let result = restore_vault(&backup, &existing);
    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(existing.join("do-not-overwrite.txt")).unwrap(),
        "original"
    );

    fs::remove_dir_all(&vault).unwrap();
    fs::remove_dir_all(backup.parent().unwrap()).unwrap();
    fs::remove_dir_all(&existing).unwrap();
}

#[test]
fn migration_wizard_end_to_end_portable_mode() {
    let source_vault = temp_dir("cistella-m2-wizard-portable-source");
    write_test_vault(&source_vault);
    let portable_root = temp_dir("cistella-m2-wizard-portable-root");
    let dirs = AppDirectories::from_test_dirs(Some(portable_root.clone()), None).unwrap();

    let vaults = vec![RecentVault {
        path: source_vault.to_string_lossy().to_string(),
        name: "Wizard Library".to_string(),
        opened_at: None,
    }];
    let migrated = migrate_vaults_to_portable(&dirs, &vaults).unwrap();

    // The migrated entry is stored as a relative path inside the portable root.
    assert_eq!(migrated[0].path, "vaults/Wizard Library");

    // Data was copied under the portable root.
    let copied = portable_root.join("vaults").join("Wizard Library");
    assert!(copied.join("manifest.json").exists());
    assert!(copied.join("tables").join("sources.parquet").exists());

    // Source remains untouched.
    assert!(source_vault.join("manifest.json").exists());

    fs::remove_dir_all(&source_vault).unwrap();
    fs::remove_dir_all(&portable_root).unwrap();
}

#[test]
fn migration_wizard_end_to_end_installed_mode() {
    let installed_dir = temp_dir("cistella-m2-wizard-installed-data");
    let source_vault = temp_dir("cistella-m2-wizard-installed-source");
    write_test_vault(&source_vault);
    let dirs = AppDirectories::from_test_dirs(None, Some(installed_dir.clone())).unwrap();

    let vaults = vec![RecentVault {
        path: source_vault.to_string_lossy().to_string(),
        name: "Installed Library".to_string(),
        opened_at: None,
    }];
    let migrated = migrate_vaults_to_installed(&dirs, &vaults).unwrap();

    // The migrated entry is returned as an absolute path.
    let target = installed_dir.join("vaults").join("Installed Library");
    assert_eq!(migrated[0].path, target.to_string_lossy().to_string());
    assert!(target.join("manifest.json").exists());

    fs::remove_dir_all(&installed_dir).unwrap();
    fs::remove_dir_all(&source_vault).unwrap();
}

#[test]
fn migration_from_portable_relative_path_resolves_correctly() {
    // Simulate a portable bundle that stores vaults with relative paths.
    let portable_root = temp_dir("cistella-m2-relative-migration");
    let source_vault = portable_root.join("vaults").join("Relative Library");
    fs::create_dir_all(&source_vault).unwrap();
    write_test_vault(&source_vault);
    let installed_dir = temp_dir("cistella-m2-relative-migration-target");
    let dirs = AppDirectories::from_test_dirs(None, Some(installed_dir.clone())).unwrap();

    // UI would resolve the relative path to absolute before calling migration.
    let absolute_source = source_vault.canonicalize().unwrap();
    let vaults = vec![RecentVault {
        path: absolute_source.to_string_lossy().to_string(),
        name: "Relative Library".to_string(),
        opened_at: None,
    }];
    let migrated = migrate_vaults_to_installed(&dirs, &vaults).unwrap();

    let target = installed_dir.join("vaults").join("Relative Library");
    assert_eq!(migrated[0].path, target.to_string_lossy().to_string());
    assert!(target.join("manifest.json").exists());

    fs::remove_dir_all(&portable_root).unwrap();
    fs::remove_dir_all(&installed_dir).unwrap();
}
