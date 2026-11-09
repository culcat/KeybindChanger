# KeybindChanger

A desktop GUI for [Hyprland](https://hypr.land) keybindings.

KeybindChanger shows every active hotkey in one place and lets you:

- **See** all bindings with their description and command.
- **Rebind** an existing hotkey (including Omarchy defaults, without editing
  system files).
- **Add** a new hotkey that launches any installed application.
- **Disable** a binding you don't need.
- **Reload** Hyprland straight from the app.

It understands both modern **Lua configs** (`~/.config/hypr/*.lua`, used by
Omarchy 5.9 / Hyprland 0.56+) and classic `hyprland.conf` files.

## Status

Early development. See [AGENTS.md](AGENTS.md) for the architecture and roadmap.

## Build

```bash
cargo build --release
./target/release/keybindchanger
```

Requirements: Rust 1.85+ (edition 2024), Wayland session with Hyprland, and the
usual `libxkbcommon` / Wayland / EGL development packages for building.

## How changes are stored

KeybindChanger never edits `/usr/share/omarchy` and never rewrites your existing
lines. Every change becomes a marked block at the end of
`~/.config/hypr/bindings.lua`:

```lua
-- >>> keybindchanger:1f3a9c2b7d4e5f60 >>>
hl.unbind("SUPER + RETURN")
o.bind("SUPER + SHIFT + T", "Terminal", "foot")
-- <<< keybindchanger:1f3a9c2b7d4e5f60 <<<
```

Deleting the override in the app restores the original binding.

## License

MIT
