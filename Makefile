# asst: CalDAV tasks. asstd syncs and reminds; asst, the bar, the window and
# quick add all talk to it.

.PHONY: help dev daemon window quick-add test nextcloud-test lint fmt build install uninstall package srcinfo clean

PREFIX  ?= $(HOME)/.local
DATADIR ?= $(or $(XDG_DATA_HOME),$(HOME)/.local/share)
UNITDIR ?= $(or $(XDG_CONFIG_HOME),$(HOME)/.config)/systemd/user
DBUSDIR ?= $(DATADIR)/dbus-1/services

help:            ## this list
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | expand -t18

dev:             ## an isolated asst on a private bus and a local Radicale (scripts/dev up, then window)
	scripts/dev up
	scripts/dev window

daemon:          ## asstd in the foreground (stop the service first)
	cargo run -p asstd

window:          ## the window, from the source tree
	cargo run -p asst-gtk

quick-add:       ## the quick-add popup, from the source tree
	cargo run -p asst-gtk -- quick-add

test:            ## unit tests: no server, no bus
	cargo test --workspace

nextcloud-test:  ## round trip against a real server, in a list it creates and removes (ASST_TEST_DAV_URL, _USERNAME, _PASSWORD)
	cargo test -p asst-core --test nextcloud -- --ignored --nocapture

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --check

fmt:
	cargo fmt

build:           ## optimized binaries -> target/release/
	cargo build --release --workspace

install: build   ## into ~/.local for this user (no sudo), Neovim plugin included
	install -Dm755 target/release/asst $(PREFIX)/bin/asst
	install -Dm755 target/release/asstd $(PREFIX)/bin/asstd
	install -Dm755 target/release/asst-gtk $(PREFIX)/bin/asst-gtk
	install -Dm644 packaging/icons/dev.jaeho.Asst.svg $(DATADIR)/icons/hicolor/scalable/apps/dev.jaeho.Asst.svg
	mkdir -p $(DATADIR)/applications
	sed 's#^Exec=asst-gtk#Exec=$(PREFIX)/bin/asst-gtk#' packaging/dev.jaeho.Asst.desktop > $(DATADIR)/applications/dev.jaeho.Asst.desktop
	mkdir -p $(UNITDIR) $(DBUSDIR)
	sed 's#/usr/bin/#$(PREFIX)/bin/#' packaging/asstd.service > $(UNITDIR)/asstd.service
	sed 's#/usr/bin/#$(PREFIX)/bin/#' packaging/dev.jaeho.Asst.Daemon.service > $(DBUSDIR)/dev.jaeho.Asst.Daemon.service
	sed 's#/usr/bin/#$(PREFIX)/bin/#' packaging/dev.jaeho.Asst.service > $(DBUSDIR)/dev.jaeho.Asst.service
	install -Dm644 nvim/plugin/asst.lua $(DATADIR)/asst/nvim/plugin/asst.lua
	install -Dm644 nvim/lua/asst.lua $(DATADIR)/asst/nvim/lua/asst.lua
	systemctl --user daemon-reload
	@echo "installed. next: systemctl --user enable --now asstd && asst login <server>"

uninstall:       ## remove the user install (config, cache and keyring entry stay)
	systemctl --user disable --now asstd 2>/dev/null || true
	rm -f $(PREFIX)/bin/asst $(PREFIX)/bin/asstd $(PREFIX)/bin/asst-gtk \
	      $(UNITDIR)/asstd.service $(DBUSDIR)/dev.jaeho.Asst.Daemon.service $(DBUSDIR)/dev.jaeho.Asst.service \
	      $(DATADIR)/applications/dev.jaeho.Asst.desktop $(DATADIR)/icons/hicolor/scalable/apps/dev.jaeho.Asst.svg
	rm -rf $(DATADIR)/asst/nvim
	systemctl --user daemon-reload

package:         ## build the Arch package
	cd packaging && makepkg -f

srcinfo:         ## regenerate packaging/.SRCINFO after a recipe change
	cd packaging && makepkg --printsrcinfo > .SRCINFO

clean:
	cargo clean
