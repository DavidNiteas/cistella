use std::{
    fs::{self, File},
    io::BufReader,
    path::Path,
};

use crate::error::{CoreError, Result};

/// Creates a zip archive of `vault_path` at `backup_path`.
///
/// The Vault's internal directory structure is preserved so the archive can be
/// restored later without reinterpreting file layouts. Symbolic links are
/// skipped to avoid escaping the Vault boundary.
pub fn backup_vault(vault_path: impl AsRef<Path>, backup_path: impl AsRef<Path>) -> Result<()> {
    let vault_path = vault_path.as_ref();
    let backup_path = backup_path.as_ref();

    if let Some(parent) = backup_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(backup_path)?;
    let mut zip = zip::ZipWriter::new(file);

    backup_dir_contents(&mut zip, vault_path, vault_path)?;

    zip.finish()?;
    Ok(())
}

fn backup_dir_contents(zip: &mut zip::ZipWriter<File>, base: &Path, dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .map_err(|_| CoreError::MigrationFailed("vault path prefix mismatch".to_string()))?;
        let relative_str = path_to_zip_name(relative)?;

        if metadata.file_type().is_symlink() {
            // Skip symlinks to avoid escaping the Vault boundary.
            continue;
        }

        if metadata.is_dir() {
            zip.add_directory(&relative_str, zip::write::FileOptions::<()>::default())?;
            backup_dir_contents(zip, base, &path)?;
        } else if metadata.is_file() {
            zip.start_file(&relative_str, zip::write::FileOptions::<()>::default())?;
            let mut file = BufReader::new(File::open(&path)?);
            std::io::copy(&mut file, zip)?;
        }
    }
    Ok(())
}

fn path_to_zip_name(path: &Path) -> Result<String> {
    let name = path
        .as_os_str()
        .to_str()
        .ok_or_else(|| CoreError::MigrationFailed("non-UTF-8 path in vault".to_string()))?;
    Ok(name.replace('\\', "/"))
}

/// Restores a Vault from a zip archive created by `backup_vault`.
///
/// Returns an error if `target_path` already exists, to prevent accidental
/// overwrites. The archive contents are extracted into `target_path`,
/// reproducing the original Vault layout.
pub fn restore_vault(backup_path: impl AsRef<Path>, target_path: impl AsRef<Path>) -> Result<()> {
    let backup_path = backup_path.as_ref();
    let target_path = target_path.as_ref();

    if target_path.exists() {
        return Err(CoreError::MigrationFailed(format!(
            "restore target already exists: {}",
            target_path.display()
        )));
    }

    let file = File::open(backup_path)?;
    let reader = BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader)?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(safe_name) = entry.enclosed_name() else {
            // Reject entries that try to escape the target directory.
            return Err(CoreError::MigrationFailed(format!(
                "unsafe archive entry: {}",
                entry.name()
            )));
        };
        let out_path = target_path.join(safe_name);

        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out_file = File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out_file)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
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
            r#"{"format_version":"0.1.0","vault_id":"backup-test","logical_schema_version":"0.1.0","created_at":"2026-08-30T00:00:00Z","source":{"name":"Test","entity":"sources","snapshot_date":null,"input_path":"input"},"tables":{}}"#,
        )
        .unwrap();
        fs::write(root.join("tables").join("sources.parquet"), b"dummy data").unwrap();
        fs::create_dir_all(root.join("user")).unwrap();
        fs::write(root.join("user").join("literature_items.json"), b"[]").unwrap();
    }

    #[test]
    fn backup_and_restore_roundtrip_preserves_files() {
        let vault = temp_dir("cistella-backup-source");
        write_test_vault(&vault);
        let backup = temp_dir("cistella-backup-archive").join("vault.zip");
        let restored = temp_dir("cistella-backup-restored");

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
            b"dummy data"
        );
        assert_eq!(
            fs::read_to_string(restored.join("user").join("literature_items.json")).unwrap(),
            "[]"
        );

        fs::remove_dir_all(&vault).unwrap();
        fs::remove_dir_all(backup.parent().unwrap()).unwrap();
        fs::remove_dir_all(&restored).unwrap();
    }

    #[test]
    fn restore_refuses_to_overwrite_existing_target() {
        let vault = temp_dir("cistella-backup-overwrite-source");
        write_test_vault(&vault);
        let backup = temp_dir("cistella-backup-overwrite-archive").join("vault.zip");
        let existing = temp_dir("cistella-backup-overwrite-existing");
        fs::create_dir_all(&existing).unwrap();
        fs::write(existing.join("already-here.txt"), b"x").unwrap();

        backup_vault(&vault, &backup).unwrap();
        let result = restore_vault(&backup, &existing);
        assert!(result.is_err());
        assert!(existing.join("already-here.txt").exists());

        fs::remove_dir_all(&vault).unwrap();
        fs::remove_dir_all(backup.parent().unwrap()).unwrap();
        fs::remove_dir_all(&existing).unwrap();
    }
}
