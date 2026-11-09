# KeybindChanger — Agent Guide

KeybindChanger is a native desktop GUI (Rust + GTK4) for browsing,
editing and creating [Hyprland](https://hypr.land) keybindings.

The primary use case is simple: **see every keybinding in one window, rebind it,
or add a new hotkey that launches an application** — without hand-editing config
files.

Hyprland 0.56+ (as shipped by Omarchy 5.9) supports a **Lua configuration**
(`~/.config/hypr/*.lua`) alongside the classic `hyprland.conf` format. This app
supports both, and prefers Lua when `~/.config/hypr/hyprland.lua` exists.

---

## Build / run / test

```bash
cargo build            # debug build
cargo run              # run the app (needs a Wayland session)
cargo test             # unit tests, no Hyprland required
cargo clippy --all-targets -- -D warnings
cargo fmt
```

System requirements for building on Linux: `gtk4` (4.10+), `libxkbcommon`,
Wayland (or X11) client libraries and a C compiler. Runtime: a Hyprland session
for full functionality; the app still opens and shows an explanatory banner
outside Hyprland.

## Layout

```
src/
  main.rs          entry point, GTK application setup
  app.rs           GTK window, list, editor dialog, key recorder, app picker
  model.rs         Binding, KeyCombo, Modifier, BindingDraft, Source
  apps.rs          .desktop application discovery (app picker)
  hypr/
    mod.rs         paths, discovery, hyprctl wrapper
    lua.rs         Lua config parser (o.bind / hl.bind / hl.unbind)
    conf.rs        classic hyprland.conf parser (bind = ...)
    store.rs       load/merge/persist bindings, managed-block writer
```

Set `KBC_CONFIG_DIR` to point the app at an alternative config directory
(useful for testing). `AGENT.MD` is a symlink to this file for compatibility.

## How bindings are represented

A `Binding` is the merged, user-facing view of one hotkey:

- `keys: KeyCombo` — normalized modifiers + key (`SUPER + SHIFT + R`)
- `description: String` — human readable purpose
- `command: Option<String>` — shell command / dispatcher source when known
- `source: Source` — `Default` (Omarchy / system), `UserConfig`, `Managed`
  (created by KeybindChanger), or `ConfFile`
- `managed_id: Option<String>` — id of the managed block, if any
- flags: `mouse`, `release`, `repeat`, `disabled`

### Loading

1. Parse the Lua sources in load order:
   - `/usr/share/omarchy/default/hypr/bindings/*.lua` (system defaults, read-only)
   - `~/.config/hypr/*.lua` (user config, e.g. `bindings.lua`)
2. Apply actions in order: `hl.unbind(keys)` removes an entry,
   `o.bind(...)` inserts/replaces by hotkey. Later definitions win.
3. Read `hyprctl binds -j` for the live, effective bind list, and match it with
   the parsed entries by hotkey to recover commands and descriptions.
4. If Hyprland is not running, fall back to the parsed list only.

Classic `.conf` mode follows the same idea: parse `bind`, `bindm`, `bindl`,
`bindr`, `binde`, `bindel`, `bindd` lines, following `source =` includes.
`hyprctl binds -j` is still preferred for the effective list.

### Saving

KeybindChanger **never edits system files** (`/usr/share/omarchy/...`) and does
not rewrite the user's existing lines. Every change is written as a managed
block appended to `~/.config/hypr/bindings.lua` (Lua mode) or
`~/.config/hypr/keybindchanger.conf` (conf mode):

```lua
-- >>> keybindchanger:1f3a9c2b7d4e5f60 >>>
hl.unbind("SUPER + RETURN")
hl.bind("SUPER + SHIFT + T", hl.dsp.exec_cmd("foot"), { description = "Terminal" })
-- <<< keybindchanger:1f3a9c2b7d4e5f60 <<<
```

Rules:

- The block id is stable for the lifetime of the override. Editing the same
  binding replaces its block in place; deleting it removes the override and
  restores the default.
- "Disable" writes a block containing only `hl.unbind("...")`.
- `hl.unbind` always precedes `hl.bind` so the original registration is removed.
- The managed block uses the native `hl.bind` API (not Omarchy's `o.bind`
  helper) so it works in any Hyprland Lua config. Plain shell commands are
  wrapped as `hl.dsp.exec_cmd("…")`; raw `hl.dsp.*` dispatchers are written
  as-is.
- Writing is atomic (temp file + rename) and only happens after a timestamped
  backup (`<file>.keybindchanger.bak`).
- After a successful write, run `hyprctl reload` and surface
  `hyprctl configerrors` if the reload fails.
- In Lua mode, make sure `~/.config/hypr/hyprland.lua` loads
  `require("hypr.bindings")`; append it only if it is missing.
- In conf mode, make sure `hyprland.conf` sources the generated file; append a
  clearly marked `source = ~/.config/hypr/keybindchanger.conf` only if missing.

## Domain rules / invariants

- Hotkeys are canonicalized as `MOD + MOD + KEY` in the order
  `SUPER, CTRL, ALT, SHIFT`, with single letters and named keys uppercased
  (`SUPER + SHIFT + R`, `SUPER + RETURN`). Matching between configs and
  `hyprctl` is done on `(sorted mods, lowercase key)`.
- Never write a bind with empty keys or an empty command. Description is
  optional and may be empty.
- Duplicate hotkeys are allowed by Hyprland but produce a warning in the UI.
- Lua strings written by the app must escape `\` and `"`.
- The Lua parser does not execute Lua. It statically understands the calls
  used by Omarchy (`o.bind`, `o.bind_toggle`, `hl.bind`, `hl.unbind`) plus
  string literals, `nil`, tables and call expressions. Anything it cannot
  understand is shown read-only instead of being guessed.
- `.desktop` `Exec=` values must have field codes (`%f`, `%U`, `%i`, …)
  stripped before being used as a command.

## Code conventions

- Rust 2024 edition; `cargo fmt` and `cargo clippy -D warnings` must pass.
- Errors: `anyhow::Result` in library-ish modules, human-readable messages for
  the UI; never panic on user config or missing files.
- UI text is English, concise, and uses monospace for hotkeys and commands.
- Keep modules small and pure where possible: parsing, merging and managed-block
  rewriting must be unit-testable without Hyprland or a real HOME.
- Add unit tests for every parser/merge/writer change. Use `tempfile` and
  explicit paths; never touch the real `~/.config/hypr` in tests.
- Commit after each self-contained step with a Conventional Commit message
  (`feat:`, `fix:`, `docs:`, `test:`, `chore:`).

## Testing checklist

- `cargo test`
- `cargo clippy --all-targets -- -D warnings`
- Run against a copy of the real config (`KBC_CONFIG_DIR` override, if set) and
  verify: load, search, edit existing default, add app launcher, disable,
  delete override, reload.

## Roadmap

- [x] Project scaffold, AGENTS.md
- [x] Lua + conf parsers and merge logic
- [x] Live bindings via `hyprctl`
- [x] Desktop app discovery
- [x] GUI: list, search, filters, edit/add/disable dialogs
- [x] Key capture widget
- [x] Managed-block writer with backup + reload
- [x] Packaging (desktop entry, icon), installation docs
