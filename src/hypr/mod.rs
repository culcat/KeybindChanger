//! Hyprland integration: config discovery, `hyprctl`, parsing and writing.

pub mod conf;
pub mod lua;
pub mod store;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::model::{Binding, KeyCombo, Mods, Source};

/// Locations used to read and write the Hyprland configuration.
#[derive(Clone, Debug)]
pub struct ConfigPaths {
    pub home: PathBuf,
    pub hypr_dir: PathBuf,
    /// Root of the Omarchy defaults (`/usr/share/omarchy`), when installed.
    pub omarchy_dir: Option<PathBuf>,
    pub lua_mode: bool,
    pub user_bindings_lua: PathBuf,
    pub user_conf: PathBuf,
}

impl ConfigPaths {
    pub fn discover() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let hypr_dir = std::env::var_os("KBC_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config/hypr"));
        Self::for_dir(home, hypr_dir)
    }

    pub fn for_dir(home: PathBuf, hypr_dir: PathBuf) -> Self {
        let omarchy_dir = std::env::var_os("OMARCHY_PATH")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| {
                let default = PathBuf::from("/usr/share/omarchy");
                default.is_dir().then_some(default)
            });

        let lua_mode =
            hypr_dir.join("hyprland.lua").exists() || hypr_dir.join("bindings.lua").exists();

        Self {
            user_bindings_lua: hypr_dir.join("bindings.lua"),
            user_conf: hypr_dir.join("keybindchanger.conf"),
            home,
            hypr_dir,
            omarchy_dir,
            lua_mode,
        }
    }

    /// The Omarchy bindings directory, if Omarchy is installed.
    pub fn omarchy_bindings_dir(&self) -> Option<PathBuf> {
        self.omarchy_dir
            .as_ref()
            .map(|root| root.join("default/hypr/bindings"))
            .filter(|dir| dir.is_dir())
    }

    /// True when `path` belongs to the read-only system defaults.
    pub fn is_system_file(&self, path: &Path) -> bool {
        if let Some(root) = &self.omarchy_dir {
            if path.starts_with(root) {
                return true;
            }
        }
        path.starts_with("/usr/share/omarchy") || path.starts_with("/usr/share/hypr")
    }

    pub fn hyprland_conf(&self) -> PathBuf {
        self.hypr_dir.join("hyprland.conf")
    }

    pub fn hyprland_lua(&self) -> PathBuf {
        self.hypr_dir.join("hyprland.lua")
    }
}

/// A bind as reported by `hyprctl binds -j`.
pub fn live_binds() -> Result<Vec<Binding>> {
    let output = Command::new("hyprctl")
        .args(["binds", "-j"])
        .output()
        .context("failed to run `hyprctl binds -j`")?;

    if !output.status.success() {
        bail!(
            "hyprctl exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let json: Value = serde_json::from_str(text.trim()).with_context(|| {
        format!(
            "invalid hyprctl JSON: {}",
            text.chars().take(200).collect::<String>()
        )
    })?;
    let array = json
        .as_array()
        .context("hyprctl binds did not return an array")?;

    let mut by_key: Vec<Binding> = Vec::new();
    for entry in array {
        if let Some(binding) = parse_live_bind(entry) {
            // The same hotkey can be registered more than once; the last
            // registration is the effective one.
            by_key.retain(|b| b.match_key() != binding.match_key());
            by_key.push(binding);
        }
    }

    Ok(by_key)
}

fn parse_live_bind(entry: &Value) -> Option<Binding> {
    let modmask = entry.get("modmask").and_then(Value::as_u64).unwrap_or(0) as u32;
    let key = entry.get("key").and_then(Value::as_str).unwrap_or_default();
    let keycode = entry.get("keycode").and_then(Value::as_u64).unwrap_or(0);
    let mouse = entry.get("mouse").and_then(Value::as_bool).unwrap_or(false);
    let submap = entry
        .get("submap")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let key_name = if !key.trim().is_empty() {
        key.to_string()
    } else if keycode > 0 {
        format!("code:{keycode}")
    } else if mouse {
        "mouse".to_string()
    } else {
        return None;
    };

    let keys = KeyCombo {
        mods: Mods::from_hypr_modmask(modmask),
        key: crate::model::normalize_key(&key_name),
    };

    let dispatcher = entry
        .get("dispatcher")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arg = entry
        .get("arg")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default();

    let (command, raw_dispatcher) = match dispatcher {
        "__lua" | "" => (None, None),
        "exec" => (Some(arg.clone()), None),
        other => (None, Some(format!("{other} {arg}").trim().to_string())),
    };

    let mut description = entry
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !submap.is_empty() {
        description = if description.is_empty() {
            format!("submap: {submap}")
        } else {
            format!("{description} (submap: {submap})")
        };
    }

    Some(Binding {
        keys,
        description,
        command,
        raw_dispatcher,
        source: Source::Default,
        managed_id: None,
        file: None,
        line: None,
        mouse,
        release: entry
            .get("release")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        repeat: entry
            .get("repeat")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

pub fn hyprland_running() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// Ask Hyprland to reload its configuration and report config errors.
pub fn reload() -> Result<String> {
    let output = Command::new("hyprctl")
        .arg("reload")
        .output()
        .context("failed to run `hyprctl reload`")?;

    if !output.status.success() {
        bail!(
            "hyprctl reload failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let errors = Command::new("hyprctl")
        .arg("configerrors")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    if errors.is_empty() || errors == "no errors" {
        Ok("Hyprland configuration reloaded".to_string())
    } else {
        Ok(format!("Reloaded, but Hyprland reported errors:\n{errors}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exec_live_bind() {
        let json = serde_json::json!({
            "modmask": 64,
            "key": "Return",
            "keycode": 0,
            "mouse": false,
            "release": false,
            "repeat": false,
            "description": "Terminal",
            "dispatcher": "exec",
            "arg": "foot"
        });
        let binding = parse_live_bind(&json).unwrap();
        assert_eq!(binding.keys.canonical(), "SUPER + RETURN");
        assert_eq!(binding.command.as_deref(), Some("foot"));
        assert_eq!(binding.description, "Terminal");
    }

    #[test]
    fn parses_lua_live_bind() {
        let json = serde_json::json!({
            "modmask": 65,
            "key": "SPACE",
            "description": "Omarchy menu",
            "dispatcher": "__lua",
            "arg": "6",
            "mouse": false
        });
        let binding = parse_live_bind(&json).unwrap();
        assert_eq!(binding.keys.canonical(), "SUPER + SHIFT + SPACE");
        assert!(binding.command.is_none());
    }

    #[test]
    fn parses_keycode_only_bind() {
        let json = serde_json::json!({
            "modmask": 0,
            "key": "",
            "keycode": 133,
            "dispatcher": "exec",
            "arg": "true"
        });
        let binding = parse_live_bind(&json).unwrap();
        assert_eq!(binding.keys.key, "code:133");
    }
}
