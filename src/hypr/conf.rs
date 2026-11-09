//! Parser for the classic `hyprland.conf` format, including `source =` includes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::model::{KeyCombo, Mods, parse_modifier};

use super::lua::{Action, BindAction, UnbindAction};

/// Parse a `hyprland.conf` and every file it sources.
pub fn parse_file(path: &Path, home: &Path) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut visited = HashSet::new();
    parse_into(path, home, &mut actions, &mut visited);
    actions
}

fn parse_into(path: &Path, home: &Path, actions: &mut Vec<Action>, visited: &mut HashSet<PathBuf>) {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(canonical) {
        return;
    }
    let Ok(src) = std::fs::read_to_string(path) else {
        return;
    };

    for (idx, raw_line) in src.lines().enumerate() {
        let line_no = idx + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        if key == "source" {
            for included in expand_source(value, home) {
                parse_into(&included, home, actions, visited);
            }
            continue;
        }

        if key == "unbind" {
            if let Some((mods, key_name)) = parse_mods_key(value) {
                actions.push(Action::Unbind(UnbindAction {
                    keys: KeyCombo::new(mods, key_name),
                    managed_id: None,
                }));
            }
            continue;
        }

        let Some(flags) = key.strip_prefix("bind") else {
            continue;
        };
        if !flags.chars().all(|c| "mrelntd".contains(c)) {
            continue;
        }
        if let Some(action) = parse_bind(flags, value, path, line_no) {
            actions.push(Action::Bind(action));
        }
    }
}

fn parse_bind(flags: &str, value: &str, path: &Path, line_no: usize) -> Option<BindAction> {
    let parts: Vec<&str> = value.split(',').map(str::trim).collect();
    if parts.len() < 3 {
        return None;
    }

    let mods = parse_mods(parts[0]);
    let keys = KeyCombo::new(mods, parts[1]);

    // `bindd` carries a description before the dispatcher.
    let has_description = flags.contains('d');
    let (description, dispatcher_idx) = if has_description {
        (parts.get(2).copied().unwrap_or_default(), 3)
    } else {
        ("", 2)
    };

    let dispatcher = parts.get(dispatcher_idx)?.trim();
    let arg = parts.get(dispatcher_idx + 1).copied().unwrap_or_default();
    if dispatcher.is_empty() {
        return None;
    }

    let (command, raw_dispatcher) = if dispatcher == "exec" || dispatcher == "execr" {
        (Some(arg.to_string()), None)
    } else {
        (None, Some(format!("{dispatcher} {arg}").trim().to_string()))
    };

    Some(BindAction {
        keys,
        description: description.to_string(),
        command,
        raw_dispatcher,
        mouse: flags.contains('m'),
        release: flags.contains('r'),
        repeat: flags.contains('e'),
        file: path.to_path_buf(),
        line: line_no,
        managed_id: None,
    })
}

fn parse_mods_key(value: &str) -> Option<(Mods, String)> {
    let (mods_raw, key) = value.split_once(',')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((parse_mods(mods_raw), key.to_string()))
}

fn parse_mods(raw: &str) -> Mods {
    let mut mods = Mods::default();
    for token in raw.split(['+', ' ', '\t']).filter(|t| !t.trim().is_empty()) {
        if let Some(modifier) = parse_modifier(token) {
            mods.insert(modifier);
        }
    }
    mods
}

/// Strip a `#` comment, respecting single/double quoted strings.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    for (idx, ch) in line.char_indices() {
        match (quote, ch) {
            (None, '\'' | '"') => quote = Some(ch),
            (Some(q), c) if c == q => quote = None,
            (None, '#') => return &line[..idx],
            _ => {}
        }
    }
    line
}

/// Expand `~`, `$HOME` and simple `*` globs in a `source =` value.
fn expand_source(raw: &str, home: &Path) -> Vec<PathBuf> {
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else if let Some(rest) = raw.strip_prefix("$HOME/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    };

    let Some(file_name) = expanded.file_name().and_then(|s| s.to_str()) else {
        return vec![expanded];
    };
    if !file_name.contains('*') {
        return vec![expanded];
    }

    let dir = expanded.parent().unwrap_or(Path::new("."));
    let pattern: Vec<char> = file_name.chars().collect();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut result: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| wildcard_match(&pattern, &name.chars().collect::<Vec<_>>()))
        })
        .map(|entry| entry.path())
        .collect();
    result.sort();
    result
}

fn wildcard_match(pattern: &[char], text: &[char]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }
    match pattern[0] {
        '*' => {
            wildcard_match(&pattern[1..], text)
                || (!text.is_empty() && wildcard_match(pattern, &text[1..]))
        }
        '?' => !text.is_empty() && wildcard_match(&pattern[1..], &text[1..]),
        c => !text.is_empty() && c == text[0] && wildcard_match(&pattern[1..], &text[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binds(actions: &[Action]) -> Vec<&BindAction> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Bind(b) => Some(b),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_classic_bind() {
        let src = "bind = SUPER SHIFT, R, exec, foot\n";
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hyprland.conf");
        std::fs::write(&file, src).unwrap();

        let actions = parse_file(&file, dir.path());
        let binds = binds(&actions);
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].keys.canonical(), "SUPER + SHIFT + R");
        assert_eq!(binds[0].command.as_deref(), Some("foot"));
    }

    #[test]
    fn parses_flags_and_non_exec_dispatchers() {
        let src = "\
bindm = SUPER, mouse:272, movewindow
bindr = SUPER, R, exec, echo released
binde = SUPER, E, resizeactive, 10 0
";
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hyprland.conf");
        std::fs::write(&file, src).unwrap();

        let actions = parse_file(&file, dir.path());
        let binds = binds(&actions);
        assert!(binds[0].mouse);
        assert!(binds[1].release);
        assert!(binds[2].repeat);
        assert!(binds[2].command.is_none());
        assert_eq!(
            binds[2].raw_dispatcher.as_deref(),
            Some("resizeactive 10 0")
        );
    }

    #[test]
    fn parses_source_includes_and_globs() {
        let dir = tempfile::tempdir().unwrap();
        let conf_dir = dir.path().join("conf.d");
        std::fs::create_dir_all(&conf_dir).unwrap();
        std::fs::write(
            dir.path().join("hyprland.conf"),
            "source = ~/.config/hypr/conf.d/*.conf\n",
        )
        .unwrap();
        // `~` is not expanded to `dir` here, so also source by absolute path.
        std::fs::write(
            dir.path().join("hyprland.conf"),
            format!("source = {}/conf.d/*.conf\n", dir.path().display()),
        )
        .unwrap();
        std::fs::write(conf_dir.join("keys.conf"), "bind = SUPER, Q, exec, kitty\n").unwrap();

        let actions = parse_file(&dir.path().join("hyprland.conf"), dir.path());
        let binds = binds(&actions);
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].keys.canonical(), "SUPER + Q");
    }

    #[test]
    fn parses_unbind_and_description_flag() {
        let src = "\
unbind = SUPER, RETURN
bindd = SUPER, T, My terminal, exec, foot
";
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hyprland.conf");
        std::fs::write(&file, src).unwrap();

        let actions = parse_file(&file, dir.path());
        let unbinds: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                Action::Unbind(u) => Some(u),
                _ => None,
            })
            .collect();
        assert_eq!(unbinds.len(), 1);
        assert_eq!(unbinds[0].keys.canonical(), "SUPER + RETURN");

        let binds = binds(&actions);
        assert_eq!(binds[0].description, "My terminal");
        assert_eq!(binds[0].command.as_deref(), Some("foot"));
    }

    #[test]
    fn ignores_comments_and_unknown_lines() {
        let src = "\
# bind = SUPER, X, exec, nope
windowrule = float, ^(pavucontrol)$
bind = SUPER, Y, exec, echo '# not a comment'
";
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hyprland.conf");
        std::fs::write(&file, src).unwrap();

        let actions = parse_file(&file, dir.path());
        let binds = binds(&actions);
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].command.as_deref(), Some("echo '# not a comment'"));
    }

    #[test]
    fn wildcard_matching() {
        let p: Vec<char> = "*.conf".chars().collect();
        assert!(wildcard_match(&p, &"a.conf".chars().collect::<Vec<_>>()));
        assert!(!wildcard_match(
            &p,
            &"a.conf.bak".chars().collect::<Vec<_>>()
        ));
    }
}
