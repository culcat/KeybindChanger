//! Loading, merging and persisting bindings.
//!
//! Read: parse Lua/conf sources, then reconcile with the live `hyprctl` list.
//! Write: only managed blocks inside user-owned files, never system defaults.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::model::{Binding, BindingDraft, DisabledBinding, KeyCombo, Source};

use super::ConfigPaths;
use super::conf;
use super::lua::{self, Action};

pub struct Store {
    pub paths: ConfigPaths,
    pub bindings: Vec<Binding>,
    pub disabled: Vec<DisabledBinding>,
    /// True when the list was reconciled with a running Hyprland.
    pub live: bool,
    pub warnings: Vec<String>,
}

impl Store {
    pub fn load() -> Self {
        Self::load_paths(ConfigPaths::discover())
    }

    pub fn load_paths(paths: ConfigPaths) -> Self {
        let live = super::live_binds();
        Self::load_paths_with(paths, Some(live))
    }

    /// Load without asking Hyprland. Used by tests and when running outside a
    /// Hyprland session.
    pub fn load_offline(paths: ConfigPaths) -> Self {
        Self::load_paths_with(paths, None)
    }

    fn load_paths_with(paths: ConfigPaths, live_binds: Option<Result<Vec<Binding>>>) -> Self {
        let mut warnings = Vec::new();
        let mut parsed: BTreeMap<String, Binding> = BTreeMap::new();
        let mut disabled: BTreeMap<String, DisabledBinding> = BTreeMap::new();

        for action in collect_actions(&paths) {
            match action {
                Action::Bind(bind) => {
                    let source = source_for(&paths, &bind.file, bind.managed_id.as_deref());
                    let binding = Binding {
                        keys: bind.keys,
                        description: bind.description,
                        command: bind.command,
                        raw_dispatcher: bind.raw_dispatcher,
                        source,
                        managed_id: bind.managed_id,
                        file: Some(bind.file),
                        line: Some(bind.line),
                        mouse: bind.mouse,
                        release: bind.release,
                        repeat: bind.repeat,
                    };
                    disabled.remove(&binding.match_key());
                    parsed.insert(binding.match_key(), binding);
                }
                Action::Unbind(unbind) => {
                    let key = unbind.keys.match_key();
                    let removed = parsed.remove(&key);
                    if removed.is_some() || unbind.managed_id.is_some() {
                        disabled.insert(
                            key,
                            DisabledBinding {
                                keys: unbind.keys,
                                description: removed
                                    .as_ref()
                                    .map(|b| b.description.clone())
                                    .unwrap_or_default(),
                                command: removed.as_ref().and_then(|b| b.command.clone()),
                                managed_id: unbind.managed_id,
                                source: source_for(&paths, &unbind.file, None),
                            },
                        );
                    }
                }
            }
        }

        let mut live = false;
        let live_result = live_binds.unwrap_or_else(|| Err(anyhow::anyhow!("no Hyprland session")));
        let bindings: Vec<Binding> = match live_result {
            Ok(live_binds) if !live_binds.is_empty() => {
                live = true;
                live_binds
                    .into_iter()
                    .map(|mut binding| {
                        if let Some(parsed) = parsed.get(&binding.match_key()) {
                            if binding.description.is_empty() {
                                binding.description = parsed.description.clone();
                            }
                            if binding.command.is_none() {
                                binding.command = parsed.command.clone();
                            }
                            if binding.raw_dispatcher.is_none() {
                                binding.raw_dispatcher = parsed.raw_dispatcher.clone();
                            }
                            binding.source = parsed.source;
                            binding.managed_id = parsed.managed_id.clone();
                            binding.file = parsed.file.clone();
                            binding.line = parsed.line;
                            binding.mouse |= parsed.mouse;
                            binding.release |= parsed.release;
                            binding.repeat |= parsed.repeat;
                        }
                        binding
                    })
                    .collect()
            }
            Ok(_) => {
                warnings.push(
                    "Hyprland is not reporting any bindings; showing the parsed configuration."
                        .to_string(),
                );
                parsed.into_values().collect()
            }
            Err(err) => {
                warnings.push(format!(
                    "Hyprland is not running (or hyprctl is unavailable): {err}. Showing the parsed configuration."
                ));
                parsed.into_values().collect()
            }
        };

        let mut bindings = bindings;
        bindings.sort_by(|a, b| {
            let mods = a.keys.mods.tokens().len().cmp(&b.keys.mods.tokens().len());
            mods.then_with(|| {
                a.keys
                    .canonical()
                    .to_lowercase()
                    .cmp(&b.keys.canonical().to_lowercase())
            })
        });

        let mut disabled: Vec<DisabledBinding> = disabled.into_values().collect();
        disabled.sort_by(|a, b| {
            a.keys
                .canonical()
                .to_lowercase()
                .cmp(&b.keys.canonical().to_lowercase())
        });

        Self {
            paths,
            bindings,
            disabled,
            live,
            warnings,
        }
    }

    /// Save (or replace) a managed override. Returns the managed block id.
    pub fn save_override(&self, managed_id: Option<&str>, draft: &BindingDraft) -> Result<String> {
        let combo = draft.validate().map_err(anyhow::Error::msg)?;
        let id = managed_id.map(str::to_string).unwrap_or_else(generate_id);

        if self.paths.lua_mode {
            let block = render_lua_block(
                &id,
                &combo,
                &draft.description,
                &draft.command,
                draft.mouse,
                draft.release,
                draft.repeat,
                true,
            );
            upsert_block(
                &self.paths.user_bindings_lua,
                &lua_markers(&id),
                &block,
                LUA_HEADER,
            )?;
            ensure_lua_entrypoint(&self.paths)?;
        } else {
            let block = render_conf_block(
                &id,
                &combo,
                &draft.command,
                draft.mouse,
                draft.release,
                draft.repeat,
                true,
            );
            upsert_block(
                &self.paths.user_conf,
                &conf_markers(&id),
                &block,
                CONF_HEADER,
            )?;
            ensure_conf_source(&self.paths)?;
        }

        Ok(id)
    }

    /// Remove a managed override, restoring the original binding.
    pub fn remove_override(&self, managed_id: &str) -> Result<()> {
        if managed_id.trim().is_empty() {
            bail!("This binding is not managed by KeybindChanger");
        }
        let (path, markers) = if self.paths.lua_mode {
            (&self.paths.user_bindings_lua, lua_markers(managed_id))
        } else {
            (&self.paths.user_conf, conf_markers(managed_id))
        };
        remove_block(path, &markers)
    }

    /// Replace a binding with an `unbind`-only managed block.
    pub fn disable(&self, binding: &Binding) -> Result<String> {
        let id = binding.managed_id.clone().unwrap_or_else(generate_id);

        if self.paths.lua_mode {
            let block = render_lua_block(
                &id,
                &binding.keys,
                &binding.description,
                "",
                binding.mouse,
                binding.release,
                binding.repeat,
                false,
            );
            upsert_block(
                &self.paths.user_bindings_lua,
                &lua_markers(&id),
                &block,
                LUA_HEADER,
            )?;
            ensure_lua_entrypoint(&self.paths)?;
        } else {
            let block = render_conf_block(
                &id,
                &binding.keys,
                "",
                binding.mouse,
                binding.release,
                binding.repeat,
                false,
            );
            upsert_block(
                &self.paths.user_conf,
                &conf_markers(&id),
                &block,
                CONF_HEADER,
            )?;
            ensure_conf_source(&self.paths)?;
        }

        Ok(id)
    }

    /// Re-create a previously disabled binding from a managed block.
    pub fn enable(&self, disabled: &DisabledBinding) -> Result<String> {
        let command = disabled.command.clone().unwrap_or_default();
        if command.trim().is_empty() {
            bail!(
                "Cannot restore this binding automatically because its command is unknown. Edit the config file instead."
            );
        }
        let draft = BindingDraft {
            keys: disabled.keys.canonical(),
            description: disabled.description.clone(),
            command,
            mouse: false,
            release: false,
            repeat: false,
        };
        self.save_override(disabled.managed_id.as_deref(), &draft)
    }

    pub fn reload(&self) -> Result<String> {
        super::reload()
    }

    pub fn config_summary(&self) -> String {
        if self.paths.lua_mode {
            format!("{}  •  Lua", self.paths.user_bindings_lua.display())
        } else {
            format!("{}  •  classic conf", self.paths.user_conf.display())
        }
    }
}

fn source_for(paths: &ConfigPaths, file: &Path, managed_id: Option<&str>) -> Source {
    if managed_id.is_some() {
        Source::Managed
    } else if paths.is_system_file(file) {
        Source::Default
    } else if paths.lua_mode {
        Source::UserConfig
    } else {
        Source::ConfFile
    }
}

fn collect_actions(paths: &ConfigPaths) -> Vec<Action> {
    if !paths.lua_mode {
        return conf::parse_file(&paths.hyprland_conf(), &paths.home);
    }

    let mut files: Vec<PathBuf> = Vec::new();

    // Omarchy defaults first, then user files, so user definitions win.
    if let Some(defaults) = paths.omarchy_bindings_dir() {
        let mut lua_files = lua_files_in(&defaults);
        files.append(&mut lua_files);
    }
    files.append(&mut lua_files_in(&paths.hypr_dir));
    files.dedup();

    let mut actions = Vec::new();
    for file in files {
        actions.extend(lua::parse_file(&file));
    }
    actions
}

fn lua_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("lua"))
        .collect();
    files.sort();
    files
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const LUA_HEADER: &str = "-- Managed by KeybindChanger. Edit these blocks through the app.\n";
const CONF_HEADER: &str = "# Managed by KeybindChanger. Edit these blocks through the app.\n";

fn lua_markers(id: &str) -> (String, String) {
    (
        format!("-- >>> keybindchanger:{id} >>>"),
        format!("-- <<< keybindchanger:{id} <<<"),
    )
}

fn conf_markers(id: &str) -> (String, String) {
    (
        format!("# >>> keybindchanger:{id} >>>"),
        format!("# <<< keybindchanger:{id} <<<"),
    )
}

#[allow(clippy::too_many_arguments)]
fn render_lua_block(
    id: &str,
    keys: &KeyCombo,
    description: &str,
    command: &str,
    mouse: bool,
    release: bool,
    repeat: bool,
    enabled: bool,
) -> String {
    let (start, end) = lua_markers(id);
    let mut out = String::new();
    let _ = writeln!(out, "{start}");
    let _ = writeln!(out, "hl.unbind({})", lua_string(&keys.canonical()));

    if enabled {
        let command = command.trim();
        let description = if description.trim().is_empty() {
            "nil".to_string()
        } else {
            lua_string(description)
        };
        let dispatcher = render_lua_dispatcher(command);
        let options = render_lua_options(mouse, release, repeat);
        let _ = writeln!(
            out,
            "o.bind({}, {}, {}{})",
            lua_string(&keys.canonical()),
            description,
            dispatcher,
            options
        );
    } else {
        let _ = writeln!(out, "-- disabled by KeybindChanger");
    }

    let _ = writeln!(out, "{end}");
    out
}

fn render_conf_block(
    id: &str,
    keys: &KeyCombo,
    command: &str,
    mouse: bool,
    release: bool,
    repeat: bool,
    enabled: bool,
) -> String {
    let (start, end) = conf_markers(id);
    let mut out = String::new();
    let _ = writeln!(out, "{start}");
    let mods = keys.mods.tokens().join(" ");
    let _ = writeln!(out, "unbind = {}, {}", mods, keys.key);

    if enabled && !command.trim().is_empty() {
        let mut flags = String::from("bind");
        if mouse {
            flags.push('m');
        }
        if release {
            flags.push('r');
        }
        if repeat {
            flags.push('e');
        }
        let (dispatcher, args) = split_conf_command(command.trim());
        if args.is_empty() {
            let _ = writeln!(out, "{flags} = {mods}, {}, {dispatcher}", keys.key);
        } else {
            let _ = writeln!(out, "{flags} = {mods}, {}, {dispatcher}, {args}", keys.key);
        }
    } else {
        let _ = writeln!(out, "# disabled by KeybindChanger");
    }

    let _ = writeln!(out, "{end}");
    out
}

fn render_lua_dispatcher(command: &str) -> String {
    if command.starts_with("hl.") || command.starts_with("__") {
        command.to_string()
    } else {
        lua_string(command)
    }
}

fn render_lua_options(mouse: bool, release: bool, repeat: bool) -> String {
    let mut options = Vec::new();
    if mouse {
        options.push("mouse = true");
    }
    if release {
        options.push("release = true");
    }
    if repeat {
        options.push("repeat = true");
    }
    if options.is_empty() {
        String::new()
    } else {
        format!(", {{ {} }}", options.join(", "))
    }
}

fn split_conf_command(command: &str) -> (&str, &str) {
    if let Some((dispatcher, args)) = command.split_once(' ') {
        if matches!(
            dispatcher,
            "exec"
                | "execr"
                | "killactive"
                | "workspace"
                | "movetoworkspace"
                | "movewindow"
                | "resizeactive"
                | "togglefloating"
                | "fullscreen"
                | "togglespecialworkspace"
        ) {
            return (dispatcher, args);
        }
    }
    ("exec", command)
}

fn lua_string(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

// ---------------------------------------------------------------------------
// File writing
// ---------------------------------------------------------------------------

/// Insert `block` in place of the existing block with the same markers, or
/// append it to the file.
fn upsert_block(path: &Path, markers: &(String, String), block: &str, header: &str) -> Result<()> {
    let existed = path.exists();
    let text = if existed {
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?
    } else {
        header.to_string()
    };

    let new_text = replace_block(&text, markers, block);
    write_atomic(path, &new_text)
}

fn replace_block(text: &str, markers: &(String, String), block: &str) -> String {
    let (start_marker, end_marker) = markers;
    let lines: Vec<&str> = text.lines().collect();

    let start = lines.iter().position(|l| l.trim() == start_marker);
    let Some(start) = start else {
        let mut out = text.to_string();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.trim().is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str(block);
        return out;
    };

    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.trim() == end_marker)
        .map(|(idx, _)| idx);

    let Some(end) = end else {
        // Broken block: append a fresh one.
        let mut out = text.to_string();
        out.push('\n');
        out.push_str(block);
        return out;
    };

    let mut out = String::new();
    for line in &lines[..start] {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(block);
    for line in &lines[end + 1..] {
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn remove_block(path: &Path, markers: &(String, String)) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let (start_marker, end_marker) = markers;
    let lines: Vec<&str> = text.lines().collect();

    let Some(start) = lines.iter().position(|l| l.trim() == start_marker) else {
        return Ok(());
    };
    let Some(end) = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| l.trim() == end_marker)
        .map(|(idx, _)| idx)
    else {
        return Ok(());
    };

    let mut out = String::new();
    for line in &lines[..start] {
        out.push_str(line);
        out.push('\n');
    }
    for line in &lines[end + 1..] {
        out.push_str(line);
        out.push('\n');
    }
    write_atomic(path, &out)
}

fn write_atomic(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }

    if path.exists() {
        let backup = PathBuf::from(format!("{}.keybindchanger.bak", path.display()));
        std::fs::copy(path, &backup)
            .with_context(|| format!("cannot back up {}", path.display()))?;
    }

    let tmp = PathBuf::from(format!("{}.kbc.tmp", path.display()));
    std::fs::write(&tmp, content).with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

fn ensure_lua_entrypoint(paths: &ConfigPaths) -> Result<()> {
    let entry = paths.hyprland_lua();
    if !entry.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&entry)
        .with_context(|| format!("cannot read {}", entry.display()))?;
    if text.contains("hypr.bindings") {
        return Ok(());
    }
    let mut text = text;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("\n-- KeybindChanger: load managed bindings\nrequire(\"hypr.bindings\")\n");
    write_atomic(&entry, &text)
}

fn ensure_conf_source(paths: &ConfigPaths) -> Result<()> {
    let entry = paths.hyprland_conf();
    if !entry.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&entry)
        .with_context(|| format!("cannot read {}", entry.display()))?;
    if text.contains("keybindchanger.conf") {
        return Ok(());
    }
    let mut text = text;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("\n# KeybindChanger\nsource = ~/.config/hypr/keybindchanger.conf\n");
    write_atomic(&entry, &text)
}

fn generate_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{:016x}", nanos ^ seq.wrapping_mul(0x9e37_79b9_7f4a_7c15))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(dir: &Path) -> ConfigPaths {
        let mut paths = ConfigPaths::for_dir(dir.to_path_buf(), dir.join(".config/hypr"));
        // Keep tests hermetic: do not pick up the real Omarchy installation.
        paths.omarchy_dir = Some(dir.join("omarchy-not-installed"));
        paths
    }

    fn write(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn replaces_and_removes_blocks() {
        let id = "abc";
        let markers = lua_markers(id);
        let text =
            "line1\n-- >>> keybindchanger:abc >>>\nold\n-- <<< keybindchanger:abc <<<\nline2\n";
        let block = "-- >>> keybindchanger:abc >>>\nnew\n-- <<< keybindchanger:abc <<<\n";
        let replaced = replace_block(text, &markers, block);
        assert!(replaced.contains("line1\n"));
        assert!(replaced.contains("new\n"));
        assert!(!replaced.contains("old"));
        assert!(replaced.contains("line2"));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bindings.lua");
        std::fs::write(&file, &replaced).unwrap();
        remove_block(&file, &markers).unwrap();
        let after = std::fs::read_to_string(&file).unwrap();
        assert!(!after.contains("keybindchanger"));
        assert!(after.contains("line1"));
        assert!(after.contains("line2"));
    }

    #[test]
    fn appends_when_block_missing() {
        let markers = lua_markers("x");
        let out = replace_block(
            "existing\n",
            &markers,
            "-- >>> keybindchanger:x >>>\n-- <<< keybindchanger:x <<<\n",
        );
        assert!(out.starts_with("existing\n\n"));
        assert!(out.contains("keybindchanger:x"));
    }

    #[test]
    fn lua_block_escapes_strings_and_supports_raw_dispatchers() {
        let combo = KeyCombo::parse("SUPER + SHIFT + S").unwrap();
        let block = render_lua_block(
            "id1",
            &combo,
            "Screenshot \"area\"",
            "grim -g \"$region\" - | swappy -f -",
            false,
            false,
            false,
            true,
        );
        assert!(block.contains(r#"o.bind("SUPER + SHIFT + S", "Screenshot \"area\"", "grim -g \"$region\" - | swappy -f -")"#));

        let raw = render_lua_block(
            "id2",
            &combo,
            "Resize",
            "hl.dsp.window.resize()",
            true,
            false,
            false,
            true,
        );
        assert!(raw.contains(
            "o.bind(\"SUPER + SHIFT + S\", \"Resize\", hl.dsp.window.resize(), { mouse = true })"
        ));
    }

    #[test]
    fn loads_lua_sources_and_markers() {
        let dir = tempfile::tempdir().unwrap();
        let hypr = dir.path().join(".config/hypr");
        write(&hypr.join("hyprland.lua"), "require(\"hypr.bindings\")\n");
        write(
            &hypr.join("bindings.lua"),
            r#"
o.bind("SUPER + RETURN", "Terminal", "foot")
-- >>> keybindchanger:zzz >>>
hl.unbind("SUPER + Q")
o.bind("SUPER + SHIFT + Q", "Quit", "kill")
-- <<< keybindchanger:zzz <<<
"#,
        );

        let store = Store::load_offline(paths_in(dir.path()));
        let return_bind = store
            .bindings
            .iter()
            .find(|b| b.keys.key == "RETURN")
            .unwrap();
        assert_eq!(return_bind.command.as_deref(), Some("foot"));
        assert_eq!(return_bind.source, Source::UserConfig);

        let managed = store
            .bindings
            .iter()
            .find(|b| b.keys.canonical() == "SUPER + SHIFT + Q")
            .unwrap();
        assert_eq!(managed.source, Source::Managed);
        assert_eq!(managed.managed_id.as_deref(), Some("zzz"));
    }

    #[test]
    fn unbind_creates_disabled_entry() {
        let dir = tempfile::tempdir().unwrap();
        let hypr = dir.path().join(".config/hypr");
        write(&hypr.join("hyprland.lua"), "require(\"hypr.bindings\")\n");
        write(
            &hypr.join("bindings.lua"),
            r#"
o.bind("SUPER + Q", "Kitty", "kitty")
hl.unbind("SUPER + Q")
"#,
        );

        let store = Store::load_offline(paths_in(dir.path()));
        assert!(store.bindings.is_empty());
        assert_eq!(store.disabled.len(), 1);
        assert_eq!(store.disabled[0].keys.canonical(), "SUPER + Q");
        assert_eq!(store.disabled[0].command.as_deref(), Some("kitty"));
    }

    #[test]
    fn save_override_writes_managed_block_and_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let hypr = dir.path().join(".config/hypr");
        write(&hypr.join("hyprland.lua"), "-- user config\n");
        write(&hypr.join("bindings.lua"), "-- my bindings\n");

        let store = Store::load_offline(paths_in(dir.path()));
        let draft = BindingDraft {
            keys: "SUPER + SHIFT + T".to_string(),
            description: "Terminal".to_string(),
            command: "foot".to_string(),
            ..Default::default()
        };
        let id = store.save_override(None, &draft).unwrap();

        let content = std::fs::read_to_string(hypr.join("bindings.lua")).unwrap();
        assert!(content.contains(&format!("keybindchanger:{id}")));
        assert!(content.contains(r#"hl.unbind("SUPER + SHIFT + T")"#));
        assert!(content.contains(r#"o.bind("SUPER + SHIFT + T", "Terminal", "foot")"#));
        assert!(hypr.join("bindings.lua.keybindchanger.bak").exists());

        let entry = std::fs::read_to_string(hypr.join("hyprland.lua")).unwrap();
        assert!(entry.contains("require(\"hypr.bindings\")"));

        // Saving again with the same id replaces rather than duplicates.
        store.save_override(Some(&id), &draft).unwrap();
        let content = std::fs::read_to_string(hypr.join("bindings.lua")).unwrap();
        assert_eq!(content.matches("keybindchanger:").count(), 2);
    }

    #[test]
    fn remove_override_restores_file() {
        let dir = tempfile::tempdir().unwrap();
        let hypr = dir.path().join(".config/hypr");
        write(&hypr.join("hyprland.lua"), "require(\"hypr.bindings\")\n");
        write(&hypr.join("bindings.lua"), "-- keep me\n");

        let store = Store::load_offline(paths_in(dir.path()));
        let draft = BindingDraft {
            keys: "SUPER + F1".to_string(),
            description: "Help".to_string(),
            command: "yelp".to_string(),
            ..Default::default()
        };
        let id = store.save_override(None, &draft).unwrap();
        store.remove_override(&id).unwrap();
        let content = std::fs::read_to_string(hypr.join("bindings.lua")).unwrap();
        assert!(!content.contains("keybindchanger"));
        assert!(content.contains("keep me"));
    }

    #[test]
    fn conf_mode_writes_and_sources() {
        let dir = tempfile::tempdir().unwrap();
        let hypr = dir.path().join(".config/hypr");
        write(
            &hypr.join("hyprland.conf"),
            "monitor = , preferred, auto, 1\n",
        );

        let store = Store::load_offline(paths_in(dir.path()));
        assert!(!store.paths.lua_mode);
        let draft = BindingDraft {
            keys: "SUPER + SHIFT + T".to_string(),
            description: String::new(),
            command: "foot".to_string(),
            ..Default::default()
        };
        let id = store.save_override(None, &draft).unwrap();

        let content = std::fs::read_to_string(hypr.join("keybindchanger.conf")).unwrap();
        assert!(content.contains("bind = SUPER SHIFT, T, exec, foot"));
        let entry = std::fs::read_to_string(hypr.join("hyprland.conf")).unwrap();
        assert!(entry.contains("source = ~/.config/hypr/keybindchanger.conf"));

        // The generated file parses back.
        let actions = conf::parse_file(&hypr.join("keybindchanger.conf"), dir.path());
        let bind = actions
            .iter()
            .find_map(|a| match a {
                Action::Bind(b) => Some(b),
                _ => None,
            })
            .unwrap();
        assert_eq!(bind.keys.canonical(), "SUPER + SHIFT + T");
        assert_eq!(bind.command.as_deref(), Some("foot"));
        assert!(content.contains(&id));
    }

    #[test]
    fn validation_rejects_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::load_offline(paths_in(dir.path()));
        let mut draft = BindingDraft {
            keys: "SUPER + SHIFT".to_string(),
            command: "foot".to_string(),
            ..Default::default()
        };
        assert!(store.save_override(None, &draft).is_err());
        draft.keys = "SUPER + T".to_string();
        draft.command.clear();
        assert!(store.save_override(None, &draft).is_err());
    }
}
