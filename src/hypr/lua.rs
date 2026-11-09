//! Static parser for Hyprland's Lua configuration format.
//!
//! We deliberately do **not** execute Lua. Instead we tokenize the config and
//! understand the handful of calls used for keybindings:
//!
//! ```lua
//! o.bind("SUPER + RETURN", "Terminal", "foot")
//! o.bind("SUPER + SHIFT + B", "Browser", { omarchy = "browser" })
//! o.bind_toggle("SUPER + V", "Clipboard", "clipboard")
//! hl.bind("SUPER + Q", hl.dsp.exec_cmd("kitty"), { description = "Terminal" })
//! hl.unbind("SUPER + SHIFT + SLASH")
//! ```
//!
//! Everything else is skipped. Unknown arguments are surfaced as raw
//! dispatchers rather than being guessed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::KeyCombo;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    Ident(String),
    Number(String),
    Table(BTreeMap<String, Value>),
    Call(String),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Number(s) | Value::Call(s) => Some(s),
            Value::Ident(s) if s == "nil" => None,
            Value::Ident(s) => Some(s),
            Value::Table(_) => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Ident(s) if s == "true" => Some(true),
            Value::Ident(s) if s == "false" => Some(false),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct BindAction {
    pub keys: KeyCombo,
    pub description: String,
    pub command: Option<String>,
    pub raw_dispatcher: Option<String>,
    pub mouse: bool,
    pub release: bool,
    pub repeat: bool,
    pub file: PathBuf,
    pub line: usize,
    pub managed_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct UnbindAction {
    pub keys: KeyCombo,
    pub managed_id: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Action {
    Bind(BindAction),
    Unbind(UnbindAction),
}

/// Read and parse one Lua file. Unreadable files yield an empty list.
pub fn parse_file(path: &Path) -> Vec<Action> {
    match std::fs::read_to_string(path) {
        Ok(src) => parse_source(&src, path),
        Err(_) => Vec::new(),
    }
}

pub fn parse_source(src: &str, path: &Path) -> Vec<Action> {
    let tokens = tokenize(src);
    let regions = managed_regions(src);

    let mut actions = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let Some(call) = match_call(&tokens, i) else {
            i += 1;
            continue;
        };

        let Some((args, close_idx)) = collect_args(&tokens, call.open_paren) else {
            i += 1;
            continue;
        };

        let line = tokens[i].line;
        let managed_id = regions
            .iter()
            .find(|(start, end, _)| line > *start && line < *end)
            .map(|(_, _, id)| id.clone());

        let raw_arg = |idx: usize| -> Option<String> {
            let (start, end) = *args.get(idx)?;
            Some(src[tokens[start].start..tokens[end].end].to_string())
        };
        let value = |idx: usize| -> Option<Value> {
            let (start, end) = *args.get(idx)?;
            parse_value(&tokens[start..=end], src)
        };

        match call.name.as_str() {
            "bind" if matches!(call.object.as_str(), "o") => {
                if let Some(action) = parse_o_bind(
                    value(0),
                    value(1),
                    value(2),
                    value(3),
                    raw_arg(2),
                    path,
                    line,
                    managed_id.clone(),
                ) {
                    actions.push(action);
                }
            }
            "bind_toggle" if matches!(call.object.as_str(), "o") => {
                if let (Some(keys), Some(toggle)) = (value(0), value(2))
                    && let Some(keys) = value_to_combo(&keys)
                {
                    let description = value(1)
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default();
                    let command = toggle.as_str().map(|t| format!("omarchy-toggle-{t}"));
                    actions.push(Action::Bind(BindAction {
                        keys,
                        description,
                        command,
                        raw_dispatcher: None,
                        mouse: false,
                        release: false,
                        repeat: false,
                        file: path.to_path_buf(),
                        line,
                        managed_id: managed_id.clone(),
                    }));
                }
            }
            "bind" if matches!(call.object.as_str(), "hl") => {
                if let Some(keys) = value(0).and_then(|v| value_to_combo(&v)) {
                    let options = value(2).and_then(|v| match v {
                        Value::Table(t) => Some(t),
                        _ => None,
                    });
                    let description = options
                        .as_ref()
                        .and_then(|t| t.get("description").or_else(|| t.get("desc")))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let dispatcher = value(1);
                    let command = dispatcher.as_ref().and_then(resolve_hl_dispatcher);
                    let raw_dispatcher = dispatcher
                        .as_ref()
                        .filter(|_| command.is_none())
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    actions.push(Action::Bind(BindAction {
                        keys,
                        description,
                        command,
                        raw_dispatcher,
                        mouse: table_bool(&options, "mouse") || table_bool(&options, "click"),
                        release: table_bool(&options, "release"),
                        repeat: table_bool(&options, "repeating") || table_bool(&options, "repeat"),
                        file: path.to_path_buf(),
                        line,
                        managed_id: managed_id.clone(),
                    }));
                }
            }
            "unbind" if matches!(call.object.as_str(), "hl") => {
                if let Some(keys) = value(0).and_then(|v| value_to_combo(&v)) {
                    actions.push(Action::Unbind(UnbindAction {
                        keys,
                        managed_id: managed_id.clone(),
                    }));
                }
            }
            _ => {}
        }

        i = close_idx + 1;
    }

    actions
}

#[allow(clippy::too_many_arguments)]
fn parse_o_bind(
    keys: Option<Value>,
    description: Option<Value>,
    dispatcher: Option<Value>,
    options: Option<Value>,
    raw_dispatcher: Option<String>,
    path: &Path,
    line: usize,
    managed_id: Option<String>,
) -> Option<Action> {
    let keys = value_to_combo(&keys?)?;
    let description = description
        .as_ref()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let options = match options {
        Some(Value::Table(t)) => Some(t),
        _ => None,
    };

    let dispatcher = dispatcher?;
    let command = resolve_o_bind(&dispatcher);
    let raw = if command.is_none() {
        raw_dispatcher.or_else(|| dispatcher.as_str().map(str::to_string))
    } else {
        None
    };

    Some(Action::Bind(BindAction {
        keys,
        description,
        command,
        raw_dispatcher: raw,
        mouse: table_bool(&options, "mouse"),
        release: table_bool(&options, "release"),
        repeat: table_bool(&options, "repeat"),
        file: path.to_path_buf(),
        line,
        managed_id,
    }))
}

fn table_bool(table: &Option<BTreeMap<String, Value>>, key: &str) -> bool {
    table
        .as_ref()
        .and_then(|t| t.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn value_to_combo(value: &Value) -> Option<KeyCombo> {
    let text = value.as_str()?;
    KeyCombo::parse(text)
}

/// Resolve the `dispatcher` argument of `o.bind`.
///
/// This mirrors Omarchy's `command_from` helper: strings are commands,
/// tables are converted to the corresponding `omarchy-launch-*` wrapper, and
/// anything else is unknown.
fn resolve_o_bind(value: &Value) -> Option<String> {
    match value {
        Value::Str(s) => Some(s.clone()),
        Value::Table(t) => {
            if let Some(omarchy) = t.get("omarchy").and_then(Value::as_str) {
                return Some(format!("omarchy-launch-{omarchy}"));
            }
            if let Some(launch) = t.get("launch").and_then(Value::as_str) {
                let command = format!("uwsm-app -- {launch}");
                return Some(match t.get("focus").and_then(Value::as_str) {
                    Some(focus) => {
                        format!(
                            "omarchy-launch-or-focus {} {}",
                            shell_quote(focus),
                            shell_quote(&command)
                        )
                    }
                    None => command,
                });
            }
            if let Some(url) = t.get("webapp").and_then(Value::as_str) {
                return Some(match t.get("focus").and_then(Value::as_str) {
                    Some(_) => format!("omarchy-launch-or-focus-webapp {}", shell_quote(url)),
                    None => format!("omarchy-launch-webapp {}", shell_quote(url)),
                });
            }
            if let Some(tui) = t.get("tui").and_then(Value::as_str) {
                return Some(
                    if t.get("focus").and_then(Value::as_bool).unwrap_or(false) {
                        format!("omarchy-launch-or-focus-tui {}", shell_quote(tui))
                    } else {
                        format!("omarchy-launch-tui {}", shell_quote(tui))
                    },
                );
            }
            None
        }
        Value::Call(raw) => extract_exec_cmd(raw),
        _ => None,
    }
}

fn resolve_hl_dispatcher(value: &Value) -> Option<String> {
    match value {
        Value::Str(s) => Some(s.clone()),
        Value::Call(raw) => extract_exec_cmd(raw),
        _ => None,
    }
}

/// `hl.dsp.exec_cmd("cmd")` (and variants) → `cmd`.
fn extract_exec_cmd(raw: &str) -> Option<String> {
    let tokens = tokenize(raw);
    let mut i = 0;
    while i < tokens.len() {
        if let Token {
            kind: TokKind::Ident(name),
            ..
        } = &tokens[i]
            && (name == "exec_cmd" || name == "exec")
        {
            // Find the opening paren, then take the first string argument.
            let mut j = i + 1;
            if matches!(
                tokens.get(j),
                Some(Token {
                    kind: TokKind::Symbol('('),
                    ..
                })
            ) {
                j += 1;
                if let Some(Token {
                    kind: TokKind::Str(s),
                    ..
                }) = tokens.get(j)
                {
                    return Some(s.clone());
                }
            }
        }
        i += 1;
    }
    None
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------------------
// Managed block regions
// ---------------------------------------------------------------------------

/// Returns `(start_line, end_line, id)` for each managed block.
fn managed_regions(src: &str) -> Vec<(usize, usize, String)> {
    let mut regions = Vec::new();
    let mut open: Option<(usize, String)> = None;

    for (idx, line) in src.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("-- >>> keybindchanger:") {
            if let Some(id) = rest.strip_suffix(">>>") {
                open = Some((line_no, id.trim().to_string()));
            }
        } else if trimmed.starts_with("-- <<< keybindchanger:")
            && let Some((start, id)) = open.take()
        {
            regions.push((start, line_no, id));
        }
    }

    regions
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum TokKind {
    Ident(String),
    Str(String),
    Number(String),
    Symbol(char),
}

#[derive(Clone, Debug, PartialEq)]
struct Token {
    kind: TokKind,
    start: usize,
    end: usize,
    line: usize,
}

/// If the cursor is on a long-bracket opener (`[[`, `[=[`, ...), consume the
/// whole construct and return `true`. Otherwise leave the cursor untouched.
fn skip_long_bracket(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    line: &mut usize,
) -> bool {
    if !matches!(chars.peek(), Some((_, '['))) {
        return false;
    }

    let mut probe = chars.clone();
    probe.next(); // first '['
    let mut equals = 0usize;
    while matches!(probe.peek(), Some((_, '='))) {
        equals += 1;
        probe.next();
    }
    if !matches!(probe.peek(), Some((_, '['))) {
        return false;
    }

    chars.next(); // first '['
    for _ in 0..equals {
        chars.next();
    }
    chars.next(); // second '['

    let closing: Vec<char> = format!("]{}]", "=".repeat(equals)).chars().collect();
    let mut matched = 0usize;
    for (_, c) in chars.by_ref() {
        if c == '\n' {
            *line += 1;
        }
        if c == closing[matched] {
            matched += 1;
            if matched == closing.len() {
                break;
            }
        } else {
            matched = usize::from(c == closing[0]);
        }
    }

    true
}

fn tokenize(src: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut chars = src.char_indices().peekable();
    let mut line = 1usize;

    while let Some((idx, ch)) = chars.next() {
        match ch {
            '\n' => {
                line += 1;
            }
            c if c.is_whitespace() => {}
            '-' if matches!(chars.peek(), Some((_, '-'))) => {
                chars.next();
                // Long comment `--[[ ... ]]` / `--[==[ ... ]==]`, otherwise a
                // line comment until the next newline.
                if !skip_long_bracket(&mut chars, &mut line) {
                    while let Some((_, c)) = chars.peek() {
                        if *c == '\n' {
                            break;
                        }
                        chars.next();
                    }
                }
            }
            '"' | '\'' => {
                let quote = ch;
                let start = idx;
                let mut value = String::new();
                while let Some((_, c)) = chars.next() {
                    if c == quote {
                        break;
                    }
                    if c == '\\' {
                        if let Some((_, esc)) = chars.next() {
                            match esc {
                                'n' => value.push('\n'),
                                't' => value.push('\t'),
                                'r' => value.push('\r'),
                                '0' => value.push('\0'),
                                '\n' => {
                                    line += 1;
                                    value.push('\n');
                                }
                                other => value.push(other),
                            }
                        }
                    } else {
                        if c == '\n' {
                            line += 1;
                        }
                        value.push(c);
                    }
                }
                let end = chars.peek().map(|(i, _)| *i).unwrap_or(src.len());
                tokens.push(Token {
                    kind: TokKind::Str(value),
                    start,
                    end,
                    line,
                });
            }
            '[' if matches!(chars.peek(), Some((_, '['))) => {
                // Long string `[[ ... ]]`.
                chars.next();
                let start = idx;
                let mut value = String::new();
                let mut closing = 0usize;
                for (_, c) in chars.by_ref() {
                    if c == '\n' {
                        line += 1;
                    }
                    if c == ']' {
                        closing += 1;
                        if closing == 2 {
                            value.pop();
                            break;
                        }
                        value.push(c);
                    } else {
                        closing = 0;
                        value.push(c);
                    }
                }
                let end = chars.peek().map(|(i, _)| *i).unwrap_or(src.len());
                tokens.push(Token {
                    kind: TokKind::Str(value),
                    start,
                    end,
                    line,
                });
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = idx;
                let mut value = String::new();
                value.push(c);
                while let Some((_, n)) = chars.peek() {
                    if n.is_ascii_alphanumeric() || *n == '_' {
                        value.push(*n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let end = chars.peek().map(|(i, _)| *i).unwrap_or(src.len());
                tokens.push(Token {
                    kind: TokKind::Ident(value),
                    start,
                    end,
                    line,
                });
            }
            c if c.is_ascii_digit() => {
                let start = idx;
                let mut value = String::new();
                value.push(c);
                while let Some((_, n)) = chars.peek() {
                    if n.is_ascii_digit() || *n == '.' {
                        value.push(*n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let end = chars.peek().map(|(i, _)| *i).unwrap_or(src.len());
                tokens.push(Token {
                    kind: TokKind::Number(value),
                    start,
                    end,
                    line,
                });
            }
            other => {
                tokens.push(Token {
                    kind: TokKind::Symbol(other),
                    start: idx,
                    end: idx + other.len_utf8(),
                    line,
                });
            }
        }
    }

    tokens
}

// ---------------------------------------------------------------------------
// Call extraction
// ---------------------------------------------------------------------------

struct Call {
    object: String,
    name: String,
    open_paren: usize,
}

fn match_call(tokens: &[Token], i: usize) -> Option<Call> {
    let TokKind::Ident(object) = &tokens.get(i)?.kind else {
        return None;
    };
    if object != "o" && object != "hl" {
        return None;
    }
    if !matches!(tokens.get(i + 1)?.kind, TokKind::Symbol('.')) {
        return None;
    }
    let TokKind::Ident(name) = &tokens.get(i + 2)?.kind else {
        return None;
    };
    if !matches!(tokens.get(i + 3)?.kind, TokKind::Symbol('(')) {
        return None;
    }
    Some(Call {
        object: object.clone(),
        name: name.clone(),
        open_paren: i + 3,
    })
}

/// Collect top-level arguments between `(` and `)`, returning token ranges.
fn collect_args(tokens: &[Token], open_paren: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let mut args = Vec::new();
    let mut depth_round = 0i32;
    let mut depth_brace = 0i32;
    let mut depth_bracket = 0i32;
    let mut arg_start = open_paren + 1;

    let mut i = open_paren + 1;
    while i < tokens.len() {
        match &tokens[i].kind {
            TokKind::Symbol('(') => depth_round += 1,
            TokKind::Symbol(')') => {
                if depth_round == 0 && depth_brace == 0 && depth_bracket == 0 {
                    if i > arg_start {
                        args.push((arg_start, i - 1));
                    }
                    return Some((args, i));
                }
                depth_round -= 1;
            }
            TokKind::Symbol('{') => depth_brace += 1,
            TokKind::Symbol('}') => depth_brace -= 1,
            TokKind::Symbol('[') => depth_bracket += 1,
            TokKind::Symbol(']') => depth_bracket -= 1,
            TokKind::Symbol(',') if depth_round == 0 && depth_brace == 0 && depth_bracket == 0 => {
                if i > arg_start {
                    args.push((arg_start, i - 1));
                }
                arg_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }

    None
}

fn parse_value(tokens: &[Token], src: &str) -> Option<Value> {
    if tokens.is_empty() {
        return None;
    }

    if tokens.len() == 1 {
        return match &tokens[0].kind {
            TokKind::Str(s) => Some(Value::Str(s.clone())),
            TokKind::Ident(s) => Some(Value::Ident(s.clone())),
            TokKind::Number(s) => Some(Value::Number(s.clone())),
            TokKind::Symbol(_) => None,
        };
    }

    if matches!(tokens[0].kind, TokKind::Symbol('{'))
        && matches!(tokens.last()?.kind, TokKind::Symbol('}'))
    {
        return Some(Value::Table(parse_table(tokens, src)));
    }

    let raw = src[tokens[0].start..tokens.last()?.end].to_string();
    Some(Value::Call(raw))
}

fn parse_table(tokens: &[Token], src: &str) -> BTreeMap<String, Value> {
    let mut table = BTreeMap::new();
    if tokens.len() < 2 {
        return table;
    }

    let mut i = 1;
    let last = tokens.len() - 1;
    while i < last {
        if matches!(tokens[i].kind, TokKind::Symbol(',')) {
            i += 1;
            continue;
        }

        let is_pair = matches!(&tokens[i].kind, TokKind::Ident(_))
            && matches!(
                tokens.get(i + 1).map(|t| &t.kind),
                Some(TokKind::Symbol('='))
            );

        if !is_pair {
            // Skip array-style values.
            i = skip_value(tokens, i, last);
            continue;
        }

        let TokKind::Ident(key) = &tokens[i].kind else {
            i += 1;
            continue;
        };
        let key = key.clone();
        let start = i + 2;
        let end = skip_value(tokens, start, last);
        if end > start
            && let Some(value) = parse_value(&tokens[start..end], src)
        {
            table.insert(key, value);
        }
        i = end;
    }

    table
}

/// Returns the index just past the value starting at `start` (stops at a
/// top-level comma or before the closing brace).
fn skip_value(tokens: &[Token], start: usize, last: usize) -> usize {
    let mut i = start;
    let mut depth = 0i32;
    while i < last {
        match &tokens[i].kind {
            TokKind::Symbol('{') | TokKind::Symbol('(') | TokKind::Symbol('[') => depth += 1,
            TokKind::Symbol('}') | TokKind::Symbol(')') | TokKind::Symbol(']') => depth -= 1,
            TokKind::Symbol(',') if depth == 0 => break,
            _ => {}
        }
        i += 1;
    }
    i
}

// Keep a couple of internals available for unit tests.
#[cfg(test)]
mod tests {
    use super::*;

    fn bindings(actions: &[Action]) -> Vec<&BindAction> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Bind(b) => Some(b),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_string_dispatcher() {
        let src = r#"o.bind("SUPER + RETURN", "Terminal", "foot")"#;
        let actions = parse_source(src, Path::new("bindings.lua"));
        let binds = bindings(&actions);
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].keys.canonical(), "SUPER + RETURN");
        assert_eq!(binds[0].description, "Terminal");
        assert_eq!(binds[0].command.as_deref(), Some("foot"));
        assert_eq!(binds[0].line, 1);
    }

    #[test]
    fn parses_table_dispatchers() {
        let src = r#"
o.bind("SUPER + SHIFT + B", "Browser", { omarchy = "browser" })
o.bind("SUPER + ALT + P", "Bitwarden", { launch = "bitwarden-desktop" })
o.bind("SUPER + SHIFT + A", "ChatGPT", { webapp = "https://chatgpt.com" })
o.bind("SUPER + SHIFT + D", "Docker", { tui = "omarchy-launch-docker-tui" })
"#;
        let actions = parse_source(src, Path::new("bindings.lua"));
        let binds = bindings(&actions);
        assert_eq!(binds[0].command.as_deref(), Some("omarchy-launch-browser"));
        assert_eq!(
            binds[1].command.as_deref(),
            Some("uwsm-app -- bitwarden-desktop")
        );
        assert_eq!(
            binds[2].command.as_deref(),
            Some("omarchy-launch-webapp 'https://chatgpt.com'")
        );
        assert_eq!(
            binds[3].command.as_deref(),
            Some("omarchy-launch-tui 'omarchy-launch-docker-tui'")
        );
    }

    #[test]
    fn parses_hl_bind_with_exec_cmd_and_description() {
        let src = r#"
hl.bind("SUPER + Q", hl.dsp.exec_cmd("kitty"), { description = "Terminal", release = true })
hl.bind("SUPER + mouse:272", hl.dsp.window.drag(), { mouse = true })
"#;
        let actions = parse_source(src, Path::new("x.lua"));
        let binds = bindings(&actions);
        assert_eq!(binds[0].command.as_deref(), Some("kitty"));
        assert_eq!(binds[0].description, "Terminal");
        assert!(binds[0].release);
        assert!(binds[1].command.is_none());
        assert!(binds[1].mouse);
        assert!(
            binds[1]
                .raw_dispatcher
                .as_deref()
                .unwrap()
                .contains("hl.dsp.window.drag")
        );
    }

    #[test]
    fn parses_unbind_and_managed_regions() {
        let src = r#"
hl.unbind("SUPER + SHIFT + SLASH")
-- >>> keybindchanger:abc123 >>>
hl.unbind("SUPER + RETURN")
o.bind("SUPER + SHIFT + T", "Terminal", "alacritty")
-- <<< keybindchanger:abc123 <<<
"#;
        let actions = parse_source(src, Path::new("bindings.lua"));
        assert_eq!(actions.len(), 3);
        match &actions[0] {
            Action::Unbind(u) => assert!(u.managed_id.is_none()),
            _ => panic!("expected unbind"),
        }
        match &actions[1] {
            Action::Unbind(u) => assert_eq!(u.managed_id.as_deref(), Some("abc123")),
            _ => panic!("expected unbind"),
        }
        match &actions[2] {
            Action::Bind(b) => {
                assert_eq!(b.managed_id.as_deref(), Some("abc123"));
                assert_eq!(b.command.as_deref(), Some("alacritty"));
            }
            _ => panic!("expected bind"),
        }
    }

    #[test]
    fn ignores_comments_and_unknown_calls() {
        let src = r#"
-- o.bind("SUPER + X", "Commented", "never")
--[[
o.bind("SUPER + Y", "Block commented", "never")
]]
o.launch_on_start("bitwarden-desktop")
o.bind("SUPER + Z", nil, "run")
"#;
        let actions = parse_source(src, Path::new("x.lua"));
        let binds = bindings(&actions);
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].keys.canonical(), "SUPER + Z");
        assert_eq!(binds[0].description, "");
    }

    #[test]
    fn handles_multiline_calls_and_escaped_strings() {
        let src = r#"
o.bind(
  "SUPER + SHIFT + S",
  "Screenshot \"area\"",
  "grim -g \"$region\" - | swappy -f -"
)
"#;
        let actions = parse_source(src, Path::new("x.lua"));
        let binds = bindings(&actions);
        assert_eq!(binds.len(), 1);
        assert_eq!(binds[0].description, "Screenshot \"area\"");
        assert_eq!(
            binds[0].command.as_deref(),
            Some("grim -g \"$region\" - | swappy -f -")
        );
        assert_eq!(binds[0].line, 2);
    }

    #[test]
    fn parses_bind_toggle() {
        let src = r#"o.bind_toggle("SUPER + SHIFT + V", "Voxtype", "voxtype")"#;
        let actions = parse_source(src, Path::new("x.lua"));
        let binds = bindings(&actions);
        assert_eq!(binds[0].command.as_deref(), Some("omarchy-toggle-voxtype"));
    }

    #[test]
    fn tokenizer_handles_quotes_commas_and_comments() {
        let tokens = tokenize(r#"o.bind("A,B", 'C', "D--not-comment") -- real"#);
        let strings: Vec<_> = tokens
            .iter()
            .filter_map(|t| match &t.kind {
                TokKind::Str(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(strings, vec!["A,B", "C", "D--not-comment"]);
    }
}
