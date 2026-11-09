PREFIX ?= /usr/local
DESTDIR ?=
BINDIR := $(DESTDIR)$(PREFIX)/bin
APPDIR := $(DESTDIR)$(PREFIX)/share/applications
ICONDIR := $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps

.PHONY: build test check install uninstall clean

build:
	cargo build --release

test:
	cargo test

check:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test

install: build
	install -Dm755 target/release/keybindchanger $(BINDIR)/keybindchanger
	install -Dm644 data/io.github.culcat.KeybindChanger.desktop \
		$(APPDIR)/io.github.culcat.KeybindChanger.desktop
	install -Dm644 data/io.github.culcat.KeybindChanger.svg \
		$(ICONDIR)/io.github.culcat.KeybindChanger.svg
	@command -v update-desktop-database >/dev/null && \
		update-desktop-database $(DESTDIR)$(PREFIX)/share/applications || true
	@command -v gtk-update-icon-cache >/dev/null && \
		gtk-update-icon-cache -f -t $(DESTDIR)$(PREFIX)/share/icons/hicolor || true

uninstall:
	rm -f $(BINDIR)/keybindchanger
	rm -f $(APPDIR)/io.github.culcat.KeybindChanger.desktop
	rm -f $(ICONDIR)/io.github.culcat.KeybindChanger.svg

clean:
	cargo clean
