//! Saved rename templates: TOML persistence + listing.
//!
//! Templates are stored in `~/.config/tonepoet/rename_templates.toml`
//! as a flat `[templates]` table mapping name → template string.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Return the path to the rename templates TOML file.
fn templates_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg)
            .join("tonepoet")
            .join("rename_templates.toml")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".config")
            .join("tonepoet")
            .join("rename_templates.toml")
    } else {
        PathBuf::from("rename_templates.toml")
    }
}

/// A rename templates file: `[templates]` section with name = "pattern" entries.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct TemplatesFile {
    #[serde(default)]
    templates: BTreeMap<String, String>,
}

/// Load all saved rename templates (sorted by name).
pub fn list_templates() -> Vec<(String, String)> {
    let path = templates_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let file: TemplatesFile = toml::from_str(&content).unwrap_or_default();
    file.templates.into_iter().collect()
}

/// Save a rename template. Creates the file and parent directories if needed.
pub fn save_template(name: &str, template: &str) -> Result<(), String> {
    let path = templates_path();

    // Load existing templates.
    let mut file: TemplatesFile = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| toml::from_str(&c).ok())
        .unwrap_or_default();

    file.templates
        .insert(name.to_string(), template.to_string());

    let toml_str = toml::to_string_pretty(&file).map_err(|e| format!("serialize error: {}", e))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {}", e))?;
    }
    std::fs::write(&path, toml_str).map_err(|e| format!("write error: {}", e))?;

    Ok(())
}

/// Delete a rename template by name.
pub fn delete_template(name: &str) -> Result<(), String> {
    let path = templates_path();
    let mut file: TemplatesFile = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| toml::from_str(&c).ok())
        .unwrap_or_default();

    if file.templates.remove(name).is_none() {
        return Err(format!("template '{}' not found", name));
    }

    let toml_str = toml::to_string_pretty(&file).map_err(|e| format!("serialize error: {}", e))?;
    std::fs::write(&path, toml_str).map_err(|e| format!("write error: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_serialize() {
        let mut file = TemplatesFile::default();
        file.templates
            .insert("standard".to_string(), "%NN% - %TITLE%".to_string());
        file.templates.insert(
            "full".to_string(),
            "%ARTIST% - %ALBUM% - %NN% - %TITLE%".to_string(),
        );

        let toml_str = toml::to_string_pretty(&file).unwrap();
        let parsed: TemplatesFile = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.templates.len(), 2);
        assert_eq!(
            parsed.templates.get("standard").map(|s| s.as_str()),
            Some("%NN% - %TITLE%")
        );
        assert_eq!(
            parsed.templates.get("full").map(|s| s.as_str()),
            Some("%ARTIST% - %ALBUM% - %NN% - %TITLE%")
        );
    }

    #[test]
    fn empty_file_parses() {
        let parsed: TemplatesFile = toml::from_str("").unwrap_or_default();
        assert!(parsed.templates.is_empty());
    }
}
