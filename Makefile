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
ICONS_DIR  ?= $(HOME)/.local/share/icons/hicolor/256x256/apps
CONFIG_DIR ?= $(HOME)/.config/kvmshare

CARGO ?= cargo
GO    ?= go

# The version baked into the Go binaries (shown in the GUI and compared
# against GitHub releases). On a tag it is exactly that tag; otherwise a
# stable dev label keeps the updater honest (dev builds always see
# published releases as newer).
VERSION ?= $(shell tag=$$(git describe --tags --exact-match 2>/dev/null); if [ -n "$$tag" ]; then echo "$$tag"; else echo v0.0.0-dev; fi)
VERSION_LDFLAGS := -X kvmshare/gui/internal/selfupdate.Version=$(VERSION)

# The GUI's build id: the same workspace fingerprint the Rust build
# script stamps into the role binaries (version + workspace members),
# computed here so one release reports one id across the whole binary
# set — the fact the install check verifies. Computing it twice (here
# for the GUI, in build.rs for Rust) keeps the two toolchains in lockstep
# without cross-toolchain linkage. scripts/build-id.sh is that
# computation — plain shell, callable by hand for verification.
BUILD_ID := $(shell $(CURDIR)/scripts/build-id.sh $(CURDIR))
BUILD_ID_LDFLAGS := -X kvmshare/gui/internal/selfupdate.BuildID=$(BUILD_ID)

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
	cd gui && $(GO_ENV) $(GO) build -tags production -ldflags "$(VERSION_LDFLAGS) $(BUILD_ID_LDFLAGS)" -o kvmshare-gui .

## Build + install into $(BINDIR), plus sample config and launcher.
install: build
	mkdir -p $(BINDIR) $(CONFIG_DIR) $(APPS_DIR) $(ICONS_DIR)
	install -m755 $(SERVER_BIN) $(BINDIR)/kvmshare-server
	install -m755 $(CLIENT_BIN) $(BINDIR)/kvmshare-client
	install -m755 $(GUI_BIN) $(BINDIR)/kvmshare-gui
	@# Manifest: sha256 of the installed set, checked by the GUI before
	@# every spawn (a mixed-version install must never run silently).
	@cd $(BINDIR) && sha256sum kvmshare-server kvmshare-client kvmshare-gui > binaries.sha256
	@# Launcher icon: hicolor theme lookup (Icon=kvmshare in the
	@# .desktop entry) — the launcher shows the real icon, not a blank
	@# default.
	install -m644 gui/assets/icon.png $(ICONS_DIR)/kvmshare.png
	@if [ ! -f $(CONFIG_DIR)/kvmshare-server.toml ]; then \
		cp kvmshare-server.toml $(CONFIG_DIR)/kvmshare-server.toml; \
		echo "  sample config -> $(CONFIG_DIR)/kvmshare-server.toml"; \
	else \
		echo "  config already present, keeping $(CONFIG_DIR)/kvmshare-server.toml"; \
	fi
	install -m644 packaging/kvmshare.desktop $(APPS_DIR)/kvmshare.desktop
	@# Launchers (dmenu, GNOME, KDE) do not reliably share this shell's
	@# PATH, so the installed entry must name the binary by absolute
	@# path — a bare Exec= is the "launcher does nothing" bug.
	@sed -i 's|^Exec=.*|Exec=$(abspath $(BINDIR))/kvmshare-gui|' $(APPS_DIR)/kvmshare.desktop
	$(MAKE) --no-print-directory ensure-input-access
	@echo "installed:"
	@echo "  $(BINDIR)/kvmshare-server"
	@echo "  $(BINDIR)/kvmshare-client"
	@echo "  $(BINDIR)/kvmshare-gui"

## Grant input-device access when it is missing — no action needed when it
## already works. Runs the installer's own root step (one privilege prompt
## via pkexec the very first time; silent forever after).
.PHONY: ensure-input-access
ensure-input-access:
	@if [ "$$(id -u)" = "0" ]; then \
		$(MAKE) --no-print-directory input-access; \
	else \
		ok=1; for d in /dev/input/event*; do \
			[ -e "$$d" ] || continue; \
			if [ ! -r "$$d" ]; then ok=0; break; fi; \
		done; \
		if [ -w /dev/uinput ]; then :; else ok=0; fi; \
		if [ "$$ok" = "1" ]; then \
			echo "  input access already granted"; \
		else \
			echo "  granting input-device access (one privilege prompt)..."; \
			$(MAKE) --no-print-directory input-access || echo "  warning: grant declined — input isolation will stay limited until granted"; \
		fi; \
	fi

## Grant the desktop user device access for both roles: read on the
## physical inputs (server isolation) and write on /dev/uinput (the
## virtual wheel). Self-elevates through pkexec (works as root directly,
## e.g. sudo make input-access).
input-access:
	cd gui && $(GO_ENV) $(GO) build -ldflags "$(VERSION_LDFLAGS) $(BUILD_ID_LDFLAGS)" -o kvmshare-install ./cmd/kvmshare-install
	cd gui && ./kvmshare-install --input-access
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
release: winres
	$(CARGO) build --release
	cd gui/frontend && npm install --no-audit --no-fund >/dev/null && npm run build
	cd gui && $(GO_ENV) $(GO) build -tags production -ldflags "$(VERSION_LDFLAGS) $(BUILD_ID_LDFLAGS)" -o kvmshare-gui .
	cd gui && $(GO_ENV) $(GO) build -ldflags "$(VERSION_LDFLAGS) $(BUILD_ID_LDFLAGS)" -o kvmshare-install ./cmd/kvmshare-install
	cd gui && $(GO_ENV) $(GO) build -ldflags "$(VERSION_LDFLAGS) $(BUILD_ID_LDFLAGS)" -o kvmshare-installer ./installer
	cd gui && GOOS=windows GOARCH=amd64 CGO_ENABLED=0 $(GO) build -tags production -ldflags "-H windowsgui $(VERSION_LDFLAGS) $(BUILD_ID_LDFLAGS)" -o kvmshare-gui.exe .
	cd gui && GOOS=windows GOARCH=amd64 CGO_ENABLED=0 $(GO) build -ldflags "$(VERSION_LDFLAGS) $(BUILD_ID_LDFLAGS)" -o kvmshare-install.exe ./cmd/kvmshare-install
	cd gui && GOOS=windows GOARCH=amd64 CGO_ENABLED=0 $(GO) build -tags production -ldflags "-H windowsgui $(VERSION_LDFLAGS) $(BUILD_ID_LDFLAGS)" -o kvmshare-installer.exe ./installer
	@rm -rf dist
	@mkdir -p dist/kvmshare_$(VERSION)_linux_amd64 dist/kvmshare_$(VERSION)_windows_amd64
	cp $(SERVER_BIN) $(CLIENT_BIN) target/release/kvmshare-wheel-daemon gui/kvmshare-gui gui/kvmshare-install gui/kvmshare-installer dist/kvmshare_$(VERSION)_linux_amd64/
	cp packaging/kvmshare.desktop dist/kvmshare_$(VERSION)_linux_amd64/
	cp gui/assets/icon.png dist/kvmshare_$(VERSION)_linux_amd64/kvmshare.png
	tar -C dist -czf dist/kvmshare_$(VERSION)_linux_amd64.tar.gz kvmshare_$(VERSION)_linux_amd64
	cp gui/kvmshare-install dist/kvmshare-install_$(VERSION)_linux_amd64
	cp gui/kvmshare-installer dist/kvmshare-installer_$(VERSION)_linux_amd64
	@# The terminal one-liners (docs §9.7) resolve the standalone
	@# installer asset for their platform and verify it against
	@# SHA256SUMS before handing off — it is the trust anchor of the
	@# curl/irm/npx path. Assets exist for the platforms actually built
	@# here (linux_amd64, windows_amd64 when mingw is present); the
	@# scripts error clearly on anything else.
	@# Sanity gate: a fresh Windows role binary must carry this
	@# release's build banner. A silent cargo failure used to leave
	@# the previous build's exe in the dist dir and ship it.
	@	if [ -n "$(MINGW)" ]; then \
		echo "mingw-w64 found — building Windows binaries"; \
		$(CARGO) build --release --target $(WIN_TARGET) || exit 1; \
		cp target/$(WIN_TARGET)/release/kvmshare-server.exe target/$(WIN_TARGET)/release/kvmshare-client.exe gui/kvmshare-gui.exe gui/kvmshare-install.exe gui/kvmshare-installer.exe dist/kvmshare_$(VERSION)_windows_amd64/ || exit 1; \
		( cd dist && zip -qr kvmshare_$(VERSION)_windows_amd64.zip kvmshare_$(VERSION)_windows_amd64 ); \
		cp gui/kvmshare-install.exe dist/kvmshare-install_$(VERSION)_windows_amd64.exe; \
		cp gui/kvmshare-installer.exe dist/kvmshare-installer_$(VERSION)_windows_amd64.exe; \
		grep -q " (build " dist/kvmshare_$(VERSION)_windows_amd64/kvmshare-server.exe || { echo "FATAL: Windows server exe has no build banner (stale cross-compile output)"; exit 1; }; \
	else \
		echo "note: x86_64-w64-mingw32-gcc not found — Windows server/client binaries omitted (install mingw-w64, then make release includes them)"; \
		rm -rf dist/kvmshare_$(VERSION)_windows_amd64; \
	fi
	@# Terminal bootstrap scripts (sh / PowerShell / npx) — see docs §9.7.
	cp packaging/install.sh packaging/install.ps1 dist/
	cp packaging/npm/kvmshare.js dist/kvmshare-npm-bootstrap.js
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
		dist/kvmshare-installer_$(VERSION)_linux_amd64 \
		$(if $(MINGW),dist/kvmshare_$(VERSION)_windows_amd64.zip dist/kvmshare-install_$(VERSION)_windows_amd64.exe dist/kvmshare-installer_$(VERSION)_windows_amd64.exe) \
		dist/install.sh dist/install.ps1 dist/kvmshare-npm-bootstrap.js \
		dist/SHA256SUMS \
		--title "kvmshare $(VERSION)" \
		--notes "Portable kvmshare release. Install from a terminal: curl -fsSL <release>/install.sh | sh (Linux/macOS), irm <release>/install.ps1 | iex (Windows), or npx github:YELrhilassi/kvmshare. The GUI installer and full archives are below."
	@echo "published $(VERSION): https://github.com/YELrhilassi/kvmshare/releases/tag/$(VERSION)"

## Regenerate the Windows icon/version resources (gui/rsrc_windows_amd64.syso
## and gui/installer/rsrc_windows_amd64.syso) from their winres.json +
## gui/assets/kvmshare.ico. Needs network for the go-winres tool the first
## time. The generated .syso files are committed, so normal builds don't
## need this. Both must be bumped on every release — the installer's
## resource is what Explorer's Properties dialog and Add/Remove show.
winres: VERSION ?= $(shell tag=$$(git describe --tags --exact-match 2>/dev/null); if [ -n "$$tag" ]; then echo "$$tag"; else echo v0.8.7; fi)
# The rewrites key on the JSON field names, not the value shape: a
# dev run writes 0.0.0-dev, which a value-shaped numeric pattern could
# never match again — the files would stay poisoned until hand-edited.
winres:
	@v=$$(echo $(VERSION) | sed 's/^v//'); \
	num=$$(echo "$$v" | sed 's/[^0-9.].*$$//; s/\.$$//'); \
	for f in gui/winres/winres.json gui/installer/winres.json gui/cmd/kvmshare-install/winres.json; do \
		sed -i "s/\(\"FileVersion\": *\"\)[^,]*/\1$$v\"/g; s/\(\"ProductVersion\": *\"\)[^,]*/\1$$v\"/g; s/\(\"file_version\": *\"\)[^,]*/\1$$num\"/g; s/\(\"product_version\": *\"\)[^,]*/\1$$num\"/g; s/\(\"version\": *\"\)[^,]*/\1$$num\"/g" $$f; \
	done
	cd gui && go run github.com/tc-hib/go-winres@v0.3.1 make --in winres/winres.json --arch amd64
	cd gui/installer && go run github.com/tc-hib/go-winres@v0.3.1 make --in winres.json --arch amd64
	cd gui/cmd/kvmshare-install && go run github.com/tc-hib/go-winres@v0.3.1 make --in winres.json --arch amd64
	@echo "  regenerated all .syso files at $(VERSION)"

## Remove build artifacts.
clean:
	$(CARGO) clean
	rm -f $(GUI_BIN) gui/kvmshare-gui.exe gui/kvmshare-install gui/kvmshare-install.exe
	rm -rf dist
	find gui -name 'rsrc_windows_amd64.syso' -delete

## Remove installed files (keeps $(CONFIG_DIR)).
uninstall:
	rm -f $(BINDIR)/kvmshare-server $(BINDIR)/kvmshare-client $(BINDIR)/kvmshare-gui $(BINDIR)/kvmshare-wheel-daemon
	rm -f $(APPS_DIR)/kvmshare.desktop
	@echo "removed kvmshare binaries and launcher (config kept at $(CONFIG_DIR))"