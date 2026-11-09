//! Core data types shared by the parser, the store and the UI.

use std::fmt;
use std::path::PathBuf;

/// A keyboard modifier as understood by Hyprland.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Super,
    Ctrl,
    Alt,
    Shift,
}

impl Modifier {
    pub const ALL: [Modifier; 4] = [
        Modifier::Super,
        Modifier::Ctrl,
        Modifier::Alt,
        Modifier::Shift,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Modifier::Super => "SUPER",
            Modifier::Ctrl => "CTRL",
            Modifier::Alt => "ALT",
            Modifier::Shift => "SHIFT",
        }
    }
}

/// A set of modifiers, kept in canonical display order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Mods {
    pub super_: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub fn contains(self, modifier: Modifier) -> bool {
        match modifier {
            Modifier::Super => self.super_,
            Modifier::Ctrl => self.ctrl,
            Modifier::Alt => self.alt,
            Modifier::Shift => self.shift,
        }
    }

    pub fn insert(&mut self, modifier: Modifier) {
        match modifier {
            Modifier::Super => self.super_ = true,
            Modifier::Ctrl => self.ctrl = true,
            Modifier::Alt => self.alt = true,
            Modifier::Shift => self.shift = true,
        }
    }

    /// Canonical modifier list, in `SUPER, CTRL, ALT, SHIFT` order.
    pub fn tokens(self) -> Vec<&'static str> {
        Modifier::ALL
            .into_iter()
            .filter(|m| self.contains(*m))
            .map(Modifier::token)
            .collect()
    }

    /// Stable key used for equality/matching, independent of display order.
    pub fn match_token(self) -> String {
        self.tokens().join(",")
    }

    /// Convert a Hyprland modifier bitmask into a [`Mods`].
    ///
    /// Hyprland uses the xkb modifier masks: SHIFT=1, CAPS=2, CTRL=4, ALT=8,
    /// MOD2=16, MOD3=32, SUPER=64, MOD5=128. Caps/Mod2/Mod3/Mod5 are ignored.
    pub fn from_hypr_modmask(mask: u32) -> Self {
        Self {
            shift: mask & 1 != 0,
            ctrl: mask & 4 != 0,
            alt: mask & 8 != 0,
            super_: mask & 64 != 0,
        }
    }
}

/// A normalized hotkey: modifiers plus one key name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub mods: Mods,
    pub key: String,
}

impl KeyCombo {
    pub fn new(mods: Mods, key: impl Into<String>) -> Self {
        Self {
            mods,
            key: normalize_key(&key.into()),
        }
    }

    /// Parse a human-written combination such as `SUPER + SHIFT + R`.
    ///
    /// Accepted separators are `+` between all tokens; the final token is the
    /// key. Returns `None` when the input has no key or an empty token.
    pub fn parse(input: &str) -> Option<Self> {
        let mut mods = Mods::default();
        let mut key: Option<String> = None;

        for raw in input.split('+') {
            let token = raw.trim();
            if token.is_empty() {
                return None;
            }
            if let Some(modifier) = parse_modifier(token) {
                mods.insert(modifier);
            } else if key.is_some() {
                return None;
            } else {
                key = Some(normalize_key(token));
            }
        }

        let key = key?;
        if key.is_empty() {
            return None;
        }
        Some(Self { mods, key })
    }

    /// Canonical display form: `SUPER + SHIFT + R`.
    pub fn canonical(&self) -> String {
        let mut parts: Vec<String> = self.mods.tokens().iter().map(|s| s.to_string()).collect();
        parts.push(self.key.clone());
        parts.join(" + ")
    }

    /// Stable match key used to correlate config entries with `hyprctl binds`.
    pub fn match_key(&self) -> String {
        format!("{}|{}", self.mods.match_token(), self.key.to_lowercase())
    }
}

impl fmt::Display for KeyCombo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// Where a binding comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Shipped defaults (read-only), e.g. `/usr/share/omarchy`.
    Default,
    /// The user's own config file.
    UserConfig,
    /// Created by KeybindChanger in a managed block.
    Managed,
    /// Classic `hyprland.conf` / sourced `.conf` file.
    ConfFile,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::UserConfig => "user",
            Source::Managed => "custom",
            Source::ConfFile => "conf",
        }
    }
}

/// One effective keybinding.
#[derive(Clone, Debug)]
pub struct Binding {
    pub keys: KeyCombo,
    pub description: String,
    /// Shell command or Hyprland dispatcher, when it could be determined.
    pub command: Option<String>,
    /// Raw dispatcher text when no command can be shown (e.g. `hl.dsp.*`).
    pub raw_dispatcher: Option<String>,
    pub source: Source,
    pub managed_id: Option<String>,
    pub file: Option<PathBuf>,
    pub line: Option<usize>,
    pub mouse: bool,
    pub release: bool,
    pub repeat: bool,
}

impl Binding {
    pub fn match_key(&self) -> String {
        self.keys.match_key()
    }

    pub fn display_source(&self) -> &'static str {
        self.source.label()
    }

    /// True when the binding launches something (as opposed to a window
    /// management dispatcher).
    pub fn is_application(&self) -> bool {
        self.command.as_deref().is_some_and(|c| {
            let c = c.trim_start();
            !c.is_empty() && !c.starts_with("hl.dsp.") && !c.starts_with("__")
        })
    }
}

/// Mutable form used by the edit dialog.
#[derive(Clone, Debug, Default)]
pub struct BindingDraft {
    pub keys: String,
    pub description: String,
    pub command: String,
    pub mouse: bool,
    pub release: bool,
    pub repeat: bool,
}

impl BindingDraft {
    pub fn from_binding(binding: &Binding) -> Self {
        Self {
            keys: binding.keys.canonical(),
            description: binding.description.clone(),
            command: binding
                .command
                .clone()
                .or_else(|| binding.raw_dispatcher.clone())
                .unwrap_or_default(),
            mouse: binding.mouse,
            release: binding.release,
            repeat: binding.repeat,
        }
    }

    pub fn combo(&self) -> Option<KeyCombo> {
        KeyCombo::parse(&self.keys)
    }

    /// Validate before writing. Returns a user-facing error message.
    pub fn validate(&self) -> Result<KeyCombo, String> {
        let combo = self
            .combo()
            .ok_or_else(|| "Hotkey is invalid — use e.g. SUPER + SHIFT + R".to_string())?;
        if self.command.trim().is_empty() {
            return Err("Command must not be empty".to_string());
        }
        Ok(combo)
    }
}

/// A binding that was explicitly disabled with `unbind`.
#[derive(Clone, Debug)]
pub struct DisabledBinding {
    pub keys: KeyCombo,
    pub description: String,
    pub command: Option<String>,
    pub managed_id: Option<String>,
}

pub fn parse_modifier(token: &str) -> Option<Modifier> {
    match token.trim().to_ascii_uppercase().as_str() {
        "SUPER" | "META" | "MOD4" | "WIN" | "WINDOWS" | "CMD" => Some(Modifier::Super),
        "CTRL" | "CONTROL" => Some(Modifier::Ctrl),
        "ALT" | "MOD1" | "OPTION" => Some(Modifier::Alt),
        "SHIFT" => Some(Modifier::Shift),
        _ => None,
    }
}

/// Normalize a key name to the spelling we display and write.
pub fn normalize_key(input: &str) -> String {
    let key = input.trim();
    if key.is_empty() {
        return String::new();
    }

    let lower = key.to_ascii_lowercase();

    let named = match lower.as_str() {
        "enter" | "return" => Some("RETURN"),
        "esc" | "escape" => Some("ESCAPE"),
        "space" | "spacebar" => Some("SPACE"),
        "tab" => Some("TAB"),
        "backspace" | "back_space" => Some("BACKSPACE"),
        "delete" | "del" => Some("DELETE"),
        "insert" | "ins" => Some("INSERT"),
        "home" => Some("HOME"),
        "end" => Some("END"),
        "page_up" | "pageup" | "prior" | "pgup" => Some("PAGE_UP"),
        "page_down" | "pagedown" | "next" | "pgdn" => Some("PAGE_DOWN"),
        "print" | "printscreen" | "print_screen" => Some("PRINT"),
        "pause" => Some("PAUSE"),
        "menu" => Some("MENU"),
        "left" | "arrowleft" => Some("LEFT"),
        "right" | "arrowright" => Some("RIGHT"),
        "up" | "arrowup" => Some("UP"),
        "down" | "arrowdown" => Some("DOWN"),
        "caps_lock" | "capslock" => Some("CAPS_LOCK"),
        "num_lock" | "numlock" => Some("NUM_LOCK"),
        "scroll_lock" | "scrolllock" => Some("SCROLL_LOCK"),
        "minus" | "-" => Some("MINUS"),
        "equal" | "=" => Some("EQUAL"),
        "comma" | "," => Some("COMMA"),
        "period" | "." => Some("PERIOD"),
        "slash" | "/" => Some("SLASH"),
        "backslash" | "\\" => Some("BACKSLASH"),
        "semicolon" | ";" => Some("SEMICOLON"),
        "apostrophe" | "quote" | "'" => Some("APOSTROPHE"),
        "grave" | "backtick" | "`" => Some("GRAVE"),
        "bracketleft" | "openbracket" | "[" => Some("BRACKETLEFT"),
        "bracketright" | "closebracket" | "]" => Some("BRACKETRIGHT"),
        _ => None,
    };

    if let Some(named) = named {
        return named.to_string();
    }

    if key.len() == 1 && key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return key.to_ascii_uppercase();
    }

    if lower.starts_with("mouse:") {
        return lower;
    }

    if lower.starts_with("xf86") {
        // Keep the conventional camel case (XF86AudioRaiseVolume).
        return key.to_string();
    }

    // F-keys and keypad names are conventionally upper case.
    if lower.starts_with('f') && lower[1..].chars().all(|c| c.is_ascii_digit()) {
        return lower.to_ascii_uppercase();
    }
    if lower.starts_with("kp_") {
        return lower.to_ascii_uppercase();
    }

    key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_combo() {
        let combo = KeyCombo::parse("SUPER + SHIFT + R").unwrap();
        assert_eq!(
            combo.mods,
            Mods {
                super_: true,
                shift: true,
                ..Mods::default()
            }
        );
        assert_eq!(combo.key, "R");
        assert_eq!(combo.canonical(), "SUPER + SHIFT + R");
    }

    #[test]
    fn reorders_modifiers_and_normalizes_case() {
        let combo = KeyCombo::parse("shift + super + r").unwrap();
        assert_eq!(combo.canonical(), "SUPER + SHIFT + R");
        assert_eq!(combo.match_key(), "SUPER,SHIFT|r");
    }

    #[test]
    fn rejects_missing_or_extra_keys() {
        assert!(KeyCombo::parse("SUPER + SHIFT").is_none());
        assert!(KeyCombo::parse("SUPER + + R").is_none());
        assert!(KeyCombo::parse("SUPER + A + B").is_none());
        assert!(KeyCombo::parse("").is_none());
    }

    #[test]
    fn modifier_aliases() {
        let combo = KeyCombo::parse("meta + control + mod1 + R").unwrap();
        assert_eq!(
            combo.mods,
            Mods {
                super_: true,
                ctrl: true,
                alt: true,
                ..Mods::default()
            }
        );
    }

    #[test]
    fn normalizes_special_keys() {
        assert_eq!(normalize_key("enter"), "RETURN");
        assert_eq!(normalize_key("Page_Up"), "PAGE_UP");
        assert_eq!(normalize_key("Prior"), "PAGE_UP");
        assert_eq!(normalize_key("f12"), "F12");
        assert_eq!(
            normalize_key("XF86AudioRaiseVolume"),
            "XF86AudioRaiseVolume"
        );
        assert_eq!(normalize_key("mouse:272"), "mouse:272");
        assert_eq!(normalize_key("-"), "MINUS");
        assert_eq!(normalize_key("z"), "Z");
    }

    #[test]
    fn hypr_modmask() {
        assert_eq!(
            Mods::from_hypr_modmask(64 | 1),
            Mods {
                super_: true,
                shift: true,
                ..Mods::default()
            }
        );
        assert_eq!(
            Mods::from_hypr_modmask(4 | 8),
            Mods {
                ctrl: true,
                alt: true,
                ..Mods::default()
            }
        );
    }
}
