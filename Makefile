# kvmshare build & dev tooling.
#
#   make build     — compile everything (release Rust + GUI)
#   make install   — copy binaries, sample config and launcher into ~/.local
#   make release   — build portable release archives for Linux + Windows
#   make publish   — tag-check, build and upload a GitHub release
#   make dev       — watch sources and rebuild + reinstall on every change
#   make test      — run the full Rust test suite
#   make clean     — remove build artifacts
#   make uninstall — remove installed files (config is kept)
#
# Binaries land in ~/.local/bin by default, which is on PATH for most
# setups — so kvmshare-server / kvmshare-client / kvmshare-gui are
# directly launchable (dmenu, rofi, terminal, ...). Override PREFIX to
# install elsewhere:  make install PREFIX=/usr/local
#
# Releases are published to GitHub (see `publish`) and pulled by the
# kvmshare-install bootstrap binary and the GUI's in-app updater — the
# whole update flow is compiled Go, no shell scripts on the receiving
# end. The release must be tagged:  git tag v0.1.0 && make publish

PREFIX     ?= $(HOME)/.local
BINDIR     ?= $(PREFIX)/bin
APPS_DIR   ?= $(HOME)/.local/share/applications
CONFIG_DIR ?= $(HOME)/.config/kvmshare

CARGO ?= cargo
GO    ?= go

# The version baked into the Go binaries (shown in the GUI and compared
# against GitHub releases). On a tag it is exactly that tag; otherwise a
# stable dev label keeps the updater honest (dev builds always see
# published releases as newer).
VERSION ?= $(shell tag=$$(git describe --tags --exact-match 2>/dev/null); if [ -n "$$tag" ]; then echo "$$tag"; else echo v0.0.0-dev; fi)
VERSION_LDFLAGS := -X kvmshare/gui/internal/selfupdate.Version=$(VERSION)

# The GUI is built with Wails v3 on GTK4/WebKitGTK 6. Its bundled C
# sources trip deprecation warnings on modern GTK headers; silence them
# so a clean build really is clean.
GO_CFLAGS ?= -Wno-deprecated-declarations
GO_ENV    := CGO_CFLAGS="$(GO_CFLAGS)"

SERVER_BIN := target/release/kvmshare-server
CLIENT_BIN := target/release/kvmshare-client
GUI_BIN    := gui/kvmshare-gui

# Windows cross-compile target for the Rust binaries. Linking the Rust
# side needs mingw-w64 (x86_64-w64-mingw32-gcc) on this host plus the
# matching rustup target; without them the release ships Linux assets only
# (the Windows GUI/installer are pure Go and always build).
WIN_TARGET := x86_64-pc-windows-gnu
MINGW      := $(shell command -v x86_64-w64-mingw32-gcc 2>/dev/null)

.PHONY: build install dev test clean uninstall release publish winres input-access

## Compile everything.
build:
	$(CARGO) build --release
	cd gui/frontend && npm install --no-audit --no-fund >/dev/null && npm run build
	cd gui && $(GO_ENV) $(GO) build -tags production -ldflags "$(VERSION_LDFLAGS)" -o kvmshare-gui .

## Build + install into $(BINDIR), plus sample config and launcher.
install: build
	mkdir -p $(BINDIR) $(CONFIG_DIR) $(APPS_DIR)
	install -m755 $(SERVER_BIN) $(BINDIR)/kvmshare-server
	install -m755 $(CLIENT_BIN) $(BINDIR)/kvmshare-client
	install -m755 $(GUI_BIN) $(BINDIR)/kvmshare-gui
	@if [ ! -f $(CONFIG_DIR)/kvmshare-server.toml ]; then \
		cp kvmshare-server.toml $(CONFIG_DIR)/kvmshare-server.toml; \
		echo "  sample config -> $(CONFIG_DIR)/kvmshare-server.toml"; \
	else \
		echo "  config already present, keeping $(CONFIG_DIR)/kvmshare-server.toml"; \
	fi
	install -m644 packaging/kvmshare.desktop $(APPS_DIR)/kvmshare.desktop
	@if [ "$$(id -u)" = "0" ]; then \
		$(MAKE) --no-print-directory input-access; \
	else \
		echo "  note: input isolation needs system access — run \"sudo make input-access\" once (the portable installer does it automatically)"; \
	fi
	@echo "installed:"
	@echo "  $(BINDIR)/kvmshare-server"
	@echo "  $(BINDIR)/kvmshare-client"
	@echo "  $(BINDIR)/kvmshare-gui"

## Grant the desktop user read access to physical input devices so the
## server can isolate them while the cursor is on a client. Installs a
## udev rule (the uaccess tag makes elogind/logind grant the active seat
## user an ACL immediately — no group dance, no re-login) and adds the
## invoking user to the input group as the non-logind fallback. Must run
## as root:  sudo make input-access
input-access:
	@if [ "$$(id -u)" != "0" ]; then \
		echo "this target must run as root:  sudo make input-access"; exit 1; \
	fi
	install -m644 packaging/99-kvmshare-input.rules /etc/udev/rules.d/99-kvmshare-input.rules
	@if [ -n "$${SUDO_USER:-}" ]; then \
		echo "adding $${SUDO_USER} to the input group (fallback for non-logind systems)"; \
		usermod -aG input "$$SUDO_USER" || true; \
	fi
	udevadm control --reload-rules
	udevadm trigger --subsystem-match=input
	@echo "input access granted — isolation engages without a restart"

## Watch sources and rebuild + reinstall on every change.
dev:
	./scripts/dev.sh

## Run the Rust and Go test suites.
test:
	$(CARGO) test --workspace
	cd gui && $(GO_ENV) $(GO) test ./...

## Build the portable release archives (Linux tarball + Windows zip, the
## standalone installers, and SHA256SUMS) into dist/.
release:
	$(CARGO) build --release
	cd gui/frontend && npm install --no-audit --no-fund >/dev/null && npm run build
	cd gui && $(GO_ENV) $(GO) build -tags production -ldflags "$(VERSION_LDFLAGS)" -o kvmshare-gui .
	cd gui && $(GO_ENV) $(GO) build -ldflags "$(VERSION_LDFLAGS)" -o kvmshare-install ./cmd/kvmshare-install
	cd gui && GOOS=windows GOARCH=amd64 CGO_ENABLED=0 $(GO) build -tags production -ldflags "-H windowsgui $(VERSION_LDFLAGS)" -o kvmshare-gui.exe .
	cd gui && GOOS=windows GOARCH=amd64 CGO_ENABLED=0 $(GO) build -ldflags "$(VERSION_LDFLAGS)" -o kvmshare-install.exe ./cmd/kvmshare-install
	@rm -rf dist
	@mkdir -p dist/kvmshare_$(VERSION)_linux_amd64 dist/kvmshare_$(VERSION)_windows_amd64
	cp $(SERVER_BIN) $(CLIENT_BIN) gui/kvmshare-gui gui/kvmshare-install dist/kvmshare_$(VERSION)_linux_amd64/
	tar -C dist -czf dist/kvmshare_$(VERSION)_linux_amd64.tar.gz kvmshare_$(VERSION)_linux_amd64
	cp gui/kvmshare-install dist/kvmshare-install_$(VERSION)_linux_amd64
	@if [ -n "$(MINGW)" ]; then \
		echo "mingw-w64 found — building Windows binaries"; \
		$(CARGO) build --release --target $(WIN_TARGET); \
		cp target/$(WIN_TARGET)/release/kvmshare-server.exe target/$(WIN_TARGET)/release/kvmshare-client.exe gui/kvmshare-gui.exe gui/kvmshare-install.exe dist/kvmshare_$(VERSION)_windows_amd64/; \
		( cd dist && zip -qr kvmshare_$(VERSION)_windows_amd64.zip kvmshare_$(VERSION)_windows_amd64 ); \
		cp gui/kvmshare-install.exe dist/kvmshare-install_$(VERSION)_windows_amd64.exe; \
	else \
		echo "note: x86_64-w64-mingw32-gcc not found — Windows server/client binaries omitted (install mingw-w64, then make release includes them)"; \
		rm -rf dist/kvmshare_$(VERSION)_windows_amd64; \
	fi
	cd dist && for f in *; do [ -f "$$f" ] && [ "$$f" != SHA256SUMS ] && sha256sum "$$f"; done > SHA256SUMS
	@echo "release $(VERSION) -> dist/"
	@ls -lh dist/

## Build and upload a GitHub release. Requires an exact git tag matching
## the version (git tag v0.1.0 && make publish).
publish: release
	@if [ "$$(git describe --tags --exact-match 2>/dev/null)" != "$(VERSION)" ]; then \
		echo "publish requires HEAD to be tagged exactly $(VERSION):"; \
		echo "  git tag $(VERSION) && git push origin $(VERSION)"; \
		exit 1; \
	fi
	gh release create $(VERSION) \
		dist/kvmshare_$(VERSION)_linux_amd64.tar.gz \
		dist/kvmshare-install_$(VERSION)_linux_amd64 \
		$(if $(MINGW),dist/kvmshare_$(VERSION)_windows_amd64.zip dist/kvmshare-install_$(VERSION)_windows_amd64.exe) \
		dist/SHA256SUMS \
		--title "kvmshare $(VERSION)" \
		--notes "Portable kvmshare release. Download the installer for your platform (or the full archive) and run it — it fetches and verifies everything itself."
	@echo "published $(VERSION): https://github.com/YELrhilassi/kvmshare/releases/tag/$(VERSION)"

## Regenerate the Windows icon/version resource (gui/rsrc_windows_amd64.syso)
## from gui/winres/winres.json + gui/assets/kvmshare.ico. Needs network for
## the go-winres tool the first time. The generated .syso is committed, so
## normal builds don't need this.
winres:
	cd gui && go run github.com/tc-hib/go-winres@v0.3.1 make --in winres/winres.json --arch amd64
	@echo "  regenerated gui/rsrc_windows_amd64.syso"

## Remove build artifacts.
clean:
	$(CARGO) clean
	rm -f $(GUI_BIN) gui/kvmshare-gui.exe gui/kvmshare-install gui/kvmshare-install.exe
	rm -rf dist

## Remove installed files (keeps $(CONFIG_DIR)).
uninstall:
	rm -f $(BINDIR)/kvmshare-server $(BINDIR)/kvmshare-client $(BINDIR)/kvmshare-gui
	rm -f $(APPS_DIR)/kvmshare.desktop
	@echo "removed kvmshare binaries and launcher (config kept at $(CONFIG_DIR))"