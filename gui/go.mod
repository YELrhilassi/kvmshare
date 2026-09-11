module kvmshare/gui

go 1.25.0

require (
	github.com/godbus/dbus/v5 v5.2.2
	github.com/pelletier/go-toml/v2 v2.4.3
	github.com/wailsapp/wails/v3 v3.0.0-beta.16
	golang.org/x/sys v0.46.0
)

require github.com/go-ole/go-ole v1.3.0 // indirect

require (
	github.com/adrg/xdg v0.5.3 // indirect
	github.com/cenkalti/backoff v2.2.1+incompatible // indirect
	github.com/coder/websocket v1.8.14 // indirect
	github.com/grandcat/zeroconf v1.0.0 // patched (see replace)
	github.com/jchv/go-winloader v0.0.0-20250406163304-c1995be93bd1 // indirect
	github.com/mattn/go-colorable v0.1.14 // indirect
	github.com/mattn/go-isatty v0.0.20 // indirect
	github.com/miekg/dns v1.1.27 // indirect
	golang.org/x/crypto v0.53.0 // indirect
	golang.org/x/net v0.56.0 // indirect
)

// grandcat/zeroconf is archived and races on Shutdown (its mainloop reads
// isShutdown while Shutdown writes it). libp2p maintains the fixed fork;
// it declares the original module path, so a replace is the canonical way
// to consume it.
replace github.com/grandcat/zeroconf => github.com/libp2p/zeroconf v1.0.0
