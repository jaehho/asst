# asst: CalDAV tasks. asstd syncs and reminds; asst, the bar, the window and
# quick add all talk to it.

.PHONY: help dev daemon window quick-add test test-live nextcloud-test lint fmt build install uninstall package srcinfo clean

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

test: test-live  ## unit tests: no server, no bus
	cargo test --workspace

test-live:       ## the install wrapper never runs a failed build
	scripts/test-asst-live

nextcloud-test:  ## round trip against a real server, in a list it creates and removes (ASST_TEST_DAV_URL, _USERNAME, _PASSWORD)
	cargo test -p asst-core --test nextcloud -- --ignored --nocapture

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --check

fmt:
	cargo fmt

build:           ## optimized binaries -> target/release/
	cargo build --release --workspace

install:         ## live wrappers in ~/.local for this user (no sudo)
	# Three names, one script. Each rebuilds its package from this tree
	# on the way in. @ROOT@ becomes the checkout that installed it.
	install -d $(PREFIX)/bin
	sed 's|^DEFAULT_ROOT="@ROOT@"|DEFAULT_ROOT="$(CURDIR)"|' scripts/asst-live > $(PREFIX)/bin/asst
	chmod 755 $(PREFIX)/bin/asst
	ln -sf asst $(PREFIX)/bin/asst-gtk
	ln -sf asst $(PREFIX)/bin/asstd
	install -Dm644 packaging/icons/dev.jaeho.Asst.svg $(DATADIR)/icons/hicolor/scalable/apps/dev.jaeho.Asst.svg
	mkdir -p $(DATADIR)/applications
	sed 's#^Exec=asst-gtk#Exec=$(PREFIX)/bin/asst-gtk#' packaging/dev.jaeho.Asst.desktop > $(DATADIR)/applications/dev.jaeho.Asst.desktop
	mkdir -p $(UNITDIR) $(DBUSDIR)
	sed 's#/usr/bin/#$(PREFIX)/bin/#' packaging/asstd.service > $(UNITDIR)/asstd.service
	sed 's#/usr/bin/#$(PREFIX)/bin/#' packaging/dev.jaeho.Asst.Daemon.service > $(DBUSDIR)/dev.jaeho.Asst.Daemon.service
	sed 's#/usr/bin/#$(PREFIX)/bin/#' packaging/dev.jaeho.Asst.service > $(DBUSDIR)/dev.jaeho.Asst.service
	systemctl --user daemon-reload
	@echo "installed. next: systemctl --user enable --now asstd && asst login <server>"

uninstall:       ## remove the user install (config, cache and keyring entry stay)
	systemctl --user disable --now asstd 2>/dev/null || true
	rm -f $(PREFIX)/bin/asst $(PREFIX)/bin/asstd $(PREFIX)/bin/asst-gtk \
	      $(UNITDIR)/asstd.service $(DBUSDIR)/dev.jaeho.Asst.Daemon.service $(DBUSDIR)/dev.jaeho.Asst.service \
	      $(DATADIR)/applications/dev.jaeho.Asst.desktop $(DATADIR)/icons/hicolor/scalable/apps/dev.jaeho.Asst.svg
	systemctl --user daemon-reload

package:         ## build the Arch package
	cd packaging && makepkg -f

srcinfo:         ## regenerate packaging/.SRCINFO after a recipe change
	cd packaging && makepkg --printsrcinfo > .SRCINFO

clean:
	cargo clean
