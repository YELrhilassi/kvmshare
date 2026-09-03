# kvmshare build & dev tooling.
#
#   make build     — compile everything (release Rust + GUI)
#   make install   — copy binaries, sample config and launcher into ~/.local
#   make dev       — watch sources and rebuild + reinstall on every change
#   make test      — run the full Rust test suite
#   make clean     — remove build artifacts
#   make uninstall — remove installed files (config is kept)
#
# Binaries land in ~/.local/bin by default, which is on PATH for most
# setups — so kvmshare-server / kvmshare-client / kvmshare-gui are
# directly launchable (dmenu, rofi, terminal, ...). Override PREFIX to
# install elsewhere:  make install PREFIX=/usr/local

PREFIX     ?= $(HOME)/.local
BINDIR     ?= $(PREFIX)/bin
APPS_DIR   ?= $(HOME)/.local/share/applications
CONFIG_DIR ?= $(HOME)/.config/kvmshare

CARGO ?= cargo
GO    ?= go

SERVER_BIN := target/release/kvmshare-server
CLIENT_BIN := target/release/kvmshare-client
GUI_BIN    := gui/kvmshare-gui

.PHONY: build install dev test clean uninstall

## Compile everything.
build:
	$(CARGO) build --release
	cd gui/frontend && npm install --no-audit --no-fund >/dev/null && npm run build
	cd gui && $(GO) build -tags "production webkit2_41" -o kvmshare-gui .

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
	@echo "installed:"
	@echo "  $(BINDIR)/kvmshare-server"
	@echo "  $(BINDIR)/kvmshare-client"
	@echo "  $(BINDIR)/kvmshare-gui"

## Watch sources and rebuild + reinstall on every change.
dev:
	./scripts/dev.sh

## Run the Rust and Go test suites.
test:
	$(CARGO) test --workspace
	cd gui && $(GO) test ./...

## Remove build artifacts.
clean:
	$(CARGO) clean
	rm -f $(GUI_BIN)

## Remove installed files (keeps $(CONFIG_DIR)).
uninstall:
	rm -f $(BINDIR)/kvmshare-server $(BINDIR)/kvmshare-client $(BINDIR)/kvmshare-gui
	rm -f $(APPS_DIR)/kvmshare.desktop
	@echo "removed kvmshare binaries and launcher (config kept at $(CONFIG_DIR))"