//! Discover installed applications from `.desktop` files for the app picker.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopApp {
    pub id: String,
    pub name: String,
    pub comment: String,
    pub exec: String,
    pub icon: Option<String>,
    pub terminal: bool,
    pub desktop_file: PathBuf,
}

impl DesktopApp {
    /// Command written into the keybinding.
    ///
    /// GUI applications are launched directly; terminal applications go through
    /// `gtk-launch` so the desktop entry's `Terminal=true` is honoured.
    pub fn launch_command(&self) -> String {
        if self.terminal {
            format!("gtk-launch {}", self.id)
        } else if self.exec.is_empty() {
            format!("gtk-launch {}", self.id)
        } else {
            self.exec.clone()
        }
    }
}

/// All installed desktop applications, sorted by name and de-duplicated.
pub fn discover() -> Vec<DesktopApp> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data_home).join("applications"));
    } else if let Some(home) = &home {
        dirs.push(home.join(".local/share/applications"));
    }
    dirs.push(PathBuf::from("/usr/local/share/applications"));
    dirs.push(PathBuf::from("/usr/share/applications"));
    if let Some(home) = &home {
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs.push(PathBuf::from("/var/lib/snapd/desktop/applications"));

    let mut seen: HashSet<String> = HashSet::new();
    let mut apps = Vec::new();

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(app) = parse_desktop_entry(&contents, &path) else {
                continue;
            };
            let key = app.exec.to_lowercase();
            if !seen.insert(key) {
                continue;
            }
            apps.push(app);
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

/// Parse the `[Desktop Entry]` section of a `.desktop` file.
pub fn parse_desktop_entry(contents: &str, path: &Path) -> Option<DesktopApp> {
    let mut in_entry = false;
    let mut name = String::new();
    let mut comment = String::new();
    let mut exec = String::new();
    let mut icon = None;
    let mut terminal = false;
    let mut no_display = false;
    let mut hidden = false;

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // Ignore localized variants such as Name[ru].
        if key.contains('[') {
            continue;
        }
        let value = unescape_desktop_value(value.trim());
        match key {
            "Name" if name.is_empty() => name = value,
            "Comment" if comment.is_empty() => comment = value,
            "GenericName" if comment.is_empty() => comment = value,
            "Exec" if exec.is_empty() => exec = value,
            "Icon" if icon.is_none() => icon = Some(value),
            "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }

    if no_display || hidden {
        return None;
    }

    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    if id.is_empty() {
        return None;
    }
    if name.is_empty() {
        name = id.clone();
    }

    let exec = strip_field_codes(&exec).trim().to_string();
    if exec.is_empty() && !terminal {
        return None;
    }

    Some(DesktopApp {
        id,
        name,
        comment,
        exec,
        icon,
        terminal,
        desktop_file: path.to_path_buf(),
    })
}

/// Apply the desktop entry escape sequences (`\s`, `\n`, `\t`, `\r`, `\\`).
fn unescape_desktop_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Remove `%f`, `%U`, `%i` … field codes from an `Exec=` value.
pub fn strip_field_codes(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(code) if "fFuUdDnNickvm".contains(code) => {}
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const DESKTOP: &str = "\
[Desktop Entry]
Type=Application
Name=Firefox
Name[ru]=Файрфокс
Comment=Web Browser
Exec=firefox %u
Icon=firefox
Terminal=false
NoDisplay=false
";

    #[test]
    fn parses_desktop_entry() {
        let app = parse_desktop_entry(
            DESKTOP,
            Path::new("/usr/share/applications/firefox.desktop"),
        )
        .unwrap();
        assert_eq!(app.id, "firefox");
        assert_eq!(app.name, "Firefox");
        assert_eq!(app.comment, "Web Browser");
        assert_eq!(app.exec, "firefox");
        assert_eq!(app.launch_command(), "firefox");
    }

    #[test]
    fn skips_hidden_and_nodisplay() {
        let hidden = "[Desktop Entry]\nName=Hidden\nExec=hidden\nHidden=true\n";
        assert!(parse_desktop_entry(hidden, Path::new("/x/hidden.desktop")).is_none());
        let nodisplay = "[Desktop Entry]\nName=No\nExec=no\nNoDisplay=true\n";
        assert!(parse_desktop_entry(nodisplay, Path::new("/x/no.desktop")).is_none());
    }

    #[test]
    fn uses_gtk_launch_for_terminal_apps() {
        let terminal = "[Desktop Entry]\nName=Htop\nExec=htop\nTerminal=true\n";
        let app = parse_desktop_entry(terminal, Path::new("/x/htop.desktop")).unwrap();
        assert_eq!(app.launch_command(), "gtk-launch htop");
    }

    #[test]
    fn strips_field_codes_and_escapes() {
        assert_eq!(
            strip_field_codes("kitty --class com.example %U %f %i"),
            "kitty --class com.example"
        );
        assert_eq!(strip_field_codes("echo %%"), "echo %");
        assert_eq!(
            unescape_desktop_value(r"Line\swith\ttab\nnewline"),
            "Line with\ttab\nnewline"
        );
    }
}
