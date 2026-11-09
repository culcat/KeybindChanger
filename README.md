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

Feature-complete for day-to-day use. See [AGENTS.md](AGENTS.md) for the
architecture and roadmap.

## Install

```bash
make install                              # system-wide, /usr/local (needs root)
make install PREFIX="$HOME/.local"        # per-user, no root required
```

This builds the release binary, installs the desktop entry and icon, so
KeybindChanger appears in your application launcher.

## Build and run

```bash
cargo run             # development build
cargo build --release # optimized build at target/release/keybindchanger
```

Requirements: Rust 1.85+ (edition 2024), GTK 4.10+ development packages, and a
Wayland session with Hyprland. The app still opens outside Hyprland and shows
the parsed configuration.

## Usage

- Search by hotkey, description or command, and filter by applications,
  window management, custom bindings or disabled bindings.
- **Edit** opens the binding editor. Press **Record** and type a combination to
  capture it — the Super key is supported. **Choose app…** lists installed
  `.desktop` applications and fills in the launch command.
- **Disable** unbinds the hotkey without deleting its original definition.
  Restore it later from the *Disabled* filter.
- **Revert** removes a KeybindChanger override and restores the original.
- The refresh button reloads the config and reports Hyprland config errors.

## How changes are stored

KeybindChanger never edits `/usr/share/omarchy` and never rewrites your existing
lines. Every change becomes a marked block at the end of
`~/.config/hypr/bindings.lua`:

```lua
-- >>> keybindchanger:1f3a9c2b7d4e5f60 >>>
hl.unbind("SUPER + RETURN")
hl.bind("SUPER + SHIFT + T", hl.dsp.exec_cmd("foot"), { description = "Terminal" })
-- <<< keybindchanger:1f3a9c2b7d4e5f60 <<<
```

Deleting the override in the app restores the original binding.

## License

MIT
