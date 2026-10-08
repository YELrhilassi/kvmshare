package main

// bindings_test.go — the contract between the webview and the Go backend.
//
// Wails binds every exported method on the registered service, so the
// wire surface used to be implicit: adding an exported method to the
// service (or to a service embedded in it) silently made it callable
// from the frontend, and nothing noticed when the frontend called a
// method that no longer existed. Both directions deserve a test.
//
// Two checks:
//
//  1. The bound surface is exactly the golden list. A new bound method
//     is a deliberate API change, not an accident of naming — and a
//     dropped one is caught here instead of by a user.
//  2. Every method the frontend's `GoApp` interface calls is actually
//     bound. This is the direction that used to fail silently: the
//     TypeScript compiles fine against a method the Go side does not
//     expose, and the call only dies at runtime in the webview.
//
// The bound surface is deliberately wider than `GoApp`: the discovery
// engine's Host interface (MachineID, ServerPort, LANAddr, ...) is
// implemented by the same type, so those methods must stay exported and
// therefore stay bound. The golden list records them so the difference
// stays visible rather than mysterious.

import (
	"bufio"
	"errors"
	"os"
	"reflect"
	"sort"
	"strings"
	"testing"
)

// testApp returns an App with its embedded service layer wired, for the
// tests that build one by hand (a bare &App{} literal panics when a
// promoted service method is called, because the embedded pointer is nil).
func testApp(a *App) *App { return a.wireServices() }

// nonBindingMethods mirrors the method names Wails v3 handles specially
// and never exposes (see the framework's internalServiceMethods).
var nonBindingMethods = map[string]bool{
	"ServiceName":     true,
	"ServiceStartup":  true,
	"ServiceShutdown": true,
	"ServeHTTP":       true,
}

// boundSurface returns every method name the frontend can call, sorted.
func boundSurface() []string {
	typ := reflect.TypeOf(&App{})
	names := make([]string, 0, typ.NumMethod())
	for i := 0; i < typ.NumMethod(); i++ {
		m := typ.Method(i)
		if m.PkgPath != "" { // unexported: Wails skips these
			continue
		}
		if nonBindingMethods[m.Name] {
			continue
		}
		names = append(names, m.Name)
	}
	sort.Strings(names)
	return names
}

// goldenBoundMethods is the full bound surface. Update it deliberately
// when the frontend contract changes.
var goldenBoundMethods = []string{
	// --- called by the frontend (GoApp) ---
	"ApplyUpdate",
	"CheckForUpdate",
	"ClearLog",
	"ClientCommand",
	"ClientRunning",
	"ClientStart",
	"ClientStatus",
	"ClientStop",
	"ConnectToServer",
	"DisableLaunchAtStartup",
	"DiscoverPeers",
	"DiscoveryStatus",
	"EnableLaunchAtStartup",
	"GetLogSettings",
	"GetMachineId",
	"GetPaths",
	"GetSettings",
	"GetVersion",
	"LaunchAtStartupEnabled",
	"ListAudioDevices",
	"ListClients",
	"ListInterfaces",
	"LoadClientAudio",
	"LoadConfig",
	"LoadMediaAudio",
	"ProbeHost",
	"RefreshDiscovery",
	"RenewKeyCapture",
	"RevokeClient",
	"RevokeServer",
	"RoleElevation",
	"SaveClientAudio",
	"SaveConfig",
	"SaveMediaAudio",
	"SendConnectRequest",
	"ServerRunning",
	"ServerStart",
	"ServerStop",
	"SetLogSettings",
	"SetSettings",
	"StartActive",
	"StartKeyCapture",
	"StopActive",
	"StopKeyCapture",
	"TailLog",
	"TestAudio",
	"TrustClient",
	"TrustServer",

	// --- internal only, but exported so the discovery engine can hold
	// this type as its Host; Wails therefore binds them too ---
	"AdvertisedRole",
	"AudioStatus",
	"AutoConnectLoop",
	"ConnectedClients",
	"LANAddr",
	"MachineID",
	"MachineName",
	"OnPairRequest",
	"ReAdvertise",
	"ServerPort",
	"SingleInstance",
	"StartDiscovery",
	"StartHiddenToTray",
	"StartNotifyWatcher",
	"StopAll",
}

func TestBoundSurfaceMatchesGolden(t *testing.T) {
	got := boundSurface()

	want := make([]string, 0, len(goldenBoundMethods))
	seen := map[string]bool{}
	for _, name := range goldenBoundMethods {
		if seen[name] { // the golden list groups by origin, so allow repeats
			continue
		}
		seen[name] = true
		want = append(want, name)
	}
	sort.Strings(want)

	if !reflect.DeepEqual(got, want) {
		t.Errorf("the bound method surface changed.\n got: %v\nwant: %v\n\n"+
			"If this is intended, update goldenBoundMethods (and the "+
			"frontend's GoApp when the method is user-facing).", got, want)
	}
}

// TestServiceNameMatchesFrontend pins the wire prefix. Wails addresses a
// bound method by `<package>.<Type>.<Method>`, and bridge.ts hardcodes
// that prefix as APP_SERVICE. Renaming the type or moving it out of
// package main would silently break every call, so the two are checked
// against each other.
//
// reflect.Type.String() is used rather than PkgPath(): a real build of a
// command reports PkgPath "main", but the test binary is compiled as a
// library and reports the module path. String() is "main.App" in both.
func TestServiceNameMatchesFrontend(t *testing.T) {
	want := reflect.TypeOf(&App{}).Elem().String()

	got, err := frontendConst("frontend/src/lib/bridge.ts", "APP_SERVICE")
	if err != nil {
		t.Fatalf("read APP_SERVICE: %v", err)
	}
	if got != want {
		t.Errorf("bridge.ts APP_SERVICE = %q but the bound service is %q — "+
			"every frontend call would fail at runtime", got, want)
	}
}

// frontendConst reads a string constant like `const NAME = "value";`
// from a TypeScript file.
func frontendConst(path, name string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()

	prefix := "const " + name + " = "
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if !strings.HasPrefix(line, prefix) {
			continue
		}
		value := strings.TrimPrefix(line, prefix)
		if i := strings.IndexByte(value, '"'); i >= 0 {
			value = value[i+1:]
		}
		if i := strings.IndexByte(value, '"'); i >= 0 {
			value = value[:i]
		}
		return value, nil
	}
	if err := sc.Err(); err != nil {
		return "", err
	}
	return "", errors.New("constant " + name + " not found in " + path)
}

// TestFrontendContractIsBound is the direction that fails silently at
// runtime: a GoApp method with no bound Go implementation compiles in
// TypeScript and only breaks when the webview calls it.
func TestFrontendContractIsBound(t *testing.T) {
	names, err := frontendGoAppMethods("frontend/src/lib/bridge.ts")
	if err != nil {
		t.Fatalf("read the frontend contract: %v", err)
	}
	if len(names) == 0 {
		t.Fatal("parsed no methods out of GoApp — the interface or the file moved")
	}

	bound := map[string]bool{}
	for _, name := range boundSurface() {
		bound[name] = true
	}
	for _, name := range names {
		if !bound[name] {
			t.Errorf("frontend calls GoApp.%s but the backend does not bind it", name)
		}
	}
}

// frontendGoAppMethods extracts the method names declared by the
// `export interface GoApp { ... }` block in the given TypeScript file.
func frontendGoAppMethods(path string) ([]string, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	var (
		out     []string
		inBlock bool
	)
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := sc.Text()
		switch {
		case strings.HasPrefix(line, "export interface GoApp {"):
			inBlock = true
			continue
		case inBlock && line == "}":
			return out, nil
		case inBlock:
			trimmed := strings.TrimSpace(line)
			// Skip blank lines and comments (both // and block-style
			// JSDoc, whose continuation lines start with *).
			if trimmed == "" || strings.HasPrefix(trimmed, "//") ||
				strings.HasPrefix(trimmed, "/*") || strings.HasPrefix(trimmed, "*") {
				continue
			}
			if i := strings.IndexByte(trimmed, '('); i > 0 {
				out = append(out, trimmed[:i])
			}
		}
	}
	if err := sc.Err(); err != nil {
		return nil, err
	}
	return nil, nil
}
