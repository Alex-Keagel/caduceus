use std::fs;
use std::path::Path;

use crate::error::MarketplaceError;
use crate::manifest::PluginManifest;

/// Copy a plugin directory from `source` into `target`.
pub fn install_from_path(source: &Path, target: &Path) -> Result<(), MarketplaceError> {
    if !source.exists() {
        return Err(MarketplaceError::NotFound(source.display().to_string()));
    }

    let manifest_path = source.join("plugin.json");
    verify_manifest(&manifest_path)?;

    let plugin_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| MarketplaceError::Invalid("cannot determine plugin name".into()))?;

    let dest = target.join(plugin_name);
    copy_dir_all(source, &dest).map_err(|e| MarketplaceError::Io(e.to_string()))?;

    Ok(())
}

/// Remove a plugin directory from `target`.
pub fn uninstall(name: &str, target: &Path) -> Result<(), MarketplaceError> {
    let plugin_dir = target.join(name);
    if !plugin_dir.exists() {
        return Err(MarketplaceError::NotFound(name.to_string()));
    }
    fs::remove_dir_all(&plugin_dir).map_err(|e| MarketplaceError::Io(e.to_string()))?;
    Ok(())
}

/// Check that `path` exists and can be parsed as a valid `PluginManifest`.
pub fn verify_manifest(path: &Path) -> Result<PluginManifest, MarketplaceError> {
    if !path.exists() {
        return Err(MarketplaceError::Invalid(format!(
            "plugin.json not found at {}",
            path.display()
        )));
    }
    PluginManifest::from_file(path)
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
