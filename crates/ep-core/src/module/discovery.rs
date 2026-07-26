use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use super::manifest::{ModuleError, ModuleManifest};

#[derive(Debug, Clone)]
pub enum DiscoveryStatus {
    Valid,
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct DiscoveredModule {
    pub manifest: Option<ModuleManifest>,
    pub path: PathBuf,
    pub status: DiscoveryStatus,
}

pub fn discover_modules(modules_dir: &Path) -> Vec<DiscoveredModule> {
    let mut results = Vec::new();

    let entries = match std::fs::read_dir(modules_dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!("cannot read modules directory {}: {e}", modules_dir.display());
            return results;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let manifest_path = path.join("module.toml");
        if !manifest_path.exists() {
            debug!("skipping {} (no module.toml)", path.display());
            continue;
        }

        match ModuleManifest::from_file(&manifest_path) {
            Ok(manifest) => {
                let status = match manifest.validate() {
                    Ok(()) => DiscoveryStatus::Valid,
                    Err(errors) => DiscoveryStatus::Invalid(errors.join("; ")),
                };
                results.push(DiscoveredModule {
                    manifest: Some(manifest),
                    path,
                    status,
                });
            }
            Err(e) => {
                let reason = match &e {
                    ModuleError::Io(io) => format!("io error: {io}"),
                    ModuleError::Parse(p) => format!("parse error: {p}"),
                    ModuleError::Validation(v) => v.join("; "),
                };
                warn!("failed to load module at {}: {reason}", path.display());
                results.push(DiscoveredModule {
                    manifest: None,
                    path,
                    status: DiscoveryStatus::Invalid(reason),
                });
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ep_test_discovery_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_discover_valid_and_invalid() {
        let dir = setup_dir("valid_invalid");

        let valid_dir = dir.join("good-module");
        fs::create_dir_all(&valid_dir).unwrap();
        fs::write(
            valid_dir.join("module.toml"),
            r#"
[module]
id = "good-module"
name = "Good"
version = "1.0.0"
description = "A valid module"
category = "asr"
genre = "test"

[runtime]
type = "python"
python_version = ">=3.10"

[compute]
backends = ["cpu"]

[interface]
type = "http"
"#,
        )
        .unwrap();

        let invalid_dir = dir.join("bad-module");
        fs::create_dir_all(&invalid_dir).unwrap();
        fs::write(invalid_dir.join("module.toml"), "not valid toml [[[").unwrap();

        let no_manifest_dir = dir.join("no-manifest");
        fs::create_dir_all(&no_manifest_dir).unwrap();

        let results = discover_modules(&dir);

        assert_eq!(results.len(), 2);

        let good = results.iter().find(|m| m.path.ends_with("good-module")).unwrap();
        assert!(matches!(good.status, DiscoveryStatus::Valid));
        assert!(good.manifest.is_some());

        let bad = results.iter().find(|m| m.path.ends_with("bad-module")).unwrap();
        assert!(matches!(bad.status, DiscoveryStatus::Invalid(_)));
        assert!(bad.manifest.is_none());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_discover_nonexistent_dir() {
        let results = discover_modules(Path::new("/nonexistent/path/modules"));
        assert!(results.is_empty());
    }

    #[test]
    fn test_discover_empty_dir() {
        let dir = setup_dir("empty");
        let empty = dir.join("empty_modules");
        fs::create_dir_all(&empty).unwrap();

        let results = discover_modules(&empty);
        assert!(results.is_empty());

        fs::remove_dir_all(&dir).ok();
    }
}
