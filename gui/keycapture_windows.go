//go:build windows

package main

// keycapture_windows.go — OS-boundary key capture for shortcut
// recording.
//
// ## Why the webview's preventDefault is not enough
//
// The chord recorder runs inside the webview. `e.preventDefault()`
// there stops the *page* from acting on a key — it cannot stop the OS.
// Super+Tab's Tab never reaches the page at all: the task switcher
// consumes the chord before the window sees it, and while Super is
// held every other keydown is pre-empted by the shell. Recording a
// chord the OS has bound requires seeing the keys **before the OS
// does** and swallowing them there — the same boundary the role
// binaries' shortcut engine already uses (WH_KEYBOARD_LL, see
// crates/platform/src/windows/capture.rs).
//
// ## The contract
//
// StartKeyCapture(token) installs the hook and returns the event name
// `keycapture:<token>`; StopKeyCapture(token) removes it. The token is
// the session: every event carries it, the hook self-expires after
// captureTTL without a renewal (a crashed page can never leave the
// machine silent), and starting with a fresh token replaces the old
// session. While a session is active, EVERY physical keyboard event is
// suppressed (the hook returns 1) — the user is recording, nothing
// else on the machine acts. Injected events (the role binaries'
// SendInput, remote-control tools) are never touched. Auto-repeat is
// detected per key so the UI can collapse it; events are otherwise raw
// (virtual key + scan code + modifier snapshot), and the frontend maps
// them to the same canonical HID ids the bindings are stored as.
//
// The hook runs on its own thread with its own message pump — the
// documented requirement of low-level hooks.

import (
	"fmt"
	"sync"
	"time"
	"unsafe"

	"github.com/wailsapp/wails/v3/pkg/application"

	"golang.org/x/sys/windows"
)

// captureTTL bounds a capture session without a renewal. Long enough
// for any deliberate recording; short enough that a crashed page
// cannot leave the keyboard dead for minutes. On lapse the hook
// notifies the page (captureExpired, a broadcast the open session's
// recorder listens for) and stops suppressing — the user regains their
// machine even if the page never returns.
const captureTTL = 2 * time.Minute

// captureExpiredEvent is the TTL-lapse notification. Carries the dead
// session's token in keyEvent.Token.
const captureExpiredEvent = "keycapture:expired"

// keyEvent is one key transition delivered to the page.
type keyEvent struct {
	Token    string `json:"token"`
	Down     bool   `json:"down"`
	Repeat   bool   `json:"repeat"`
	VK       uint32 `json:"vk"`
	Scan     uint32 `json:"scan"`
	Extended bool   `json:"extended"`
	Control  bool   `json:"control"`
	Alt      bool   `json:"alt"`
	Shift    bool   `json:"shift"`
	Meta     bool   `json:"meta"`
}

// captureSession is one active recording session.
type captureSession struct {
	token   string
	expires time.Time
}

// keyCaptureState owns the at-most-one session. The hook thread reads
// it per event; the GUI thread writes it.
type keyCaptureState struct {
	mu      sync.Mutex
	session *captureSession
	app     *application.EventManager
}

var keyCapture = &keyCaptureState{}

// StartKeyCapture begins a capture session. Returns the event name the
// page subscribes to. Bound for the frontend.
func (a *App) StartKeyCapture(token string) (string, error) {
	if token == "" {
		return "", fmt.Errorf("capture token required")
	}
	a.mu.Lock()
	em := a.events
	a.mu.Unlock()
	if em == nil {
		return "", fmt.Errorf("app not ready")
	}

	keyCapture.mu.Lock()
	keyCapture.app = em
	keyCapture.session = &captureSession{token: token, expires: time.Now().Add(captureTTL)}
	keyCapture.mu.Unlock()

	if err := ensureHook(); err != nil {
		// Roll the session back: a hook that cannot exist must not
		// leave suppression armed with no one feeding the page.
		keyCapture.mu.Lock()
		keyCapture.session = nil
		keyCapture.mu.Unlock()
		return "", err
	}
	return "keycapture:" + token, nil
}

// StopKeyCapture ends the session with this token (a stale token is a
// no-op, not an error). Bound for the frontend.
func (a *App) StopKeyCapture(token string) error {
	keyCapture.mu.Lock()
	if keyCapture.session != nil && keyCapture.session.token == token {
		keyCapture.session = nil
	}
	keyCapture.mu.Unlock()
	return stopHook()
}

// RenewKeyCapture extends the session (the recorder renews on every
// received key, so a long thoughtful pause then a key still works).
// Bound for the frontend.
func (a *App) RenewKeyCapture(token string) error {
	keyCapture.mu.Lock()
	defer keyCapture.mu.Unlock()
	if keyCapture.session != nil && keyCapture.session.token == token {
		keyCapture.session.expires = time.Now().Add(captureTTL)
	}
	return nil
}

// stopHook removes the hook once no session is live. Harmless when
// already removed.
func stopHook() error {
	keyCapture.mu.Lock()
	live := keyCapture.session != nil
	keyCapture.mu.Unlock()
	if live {
		return nil // a newer recording session took the hook over
	}
	hookMu.Lock()
	defer hookMu.Unlock()
	if hookHandle == 0 {
		return nil
	}
	if hookThreadHwnd != 0 {
		procPostMessageW.Call(hookThreadHwnd, hookStopMsg, 0, 0)
	}
	for i := 0; i < 50 && hookHandle != 0; i++ {
		hookMu.Unlock()
		time.Sleep(10 * time.Millisecond)
		hookMu.Lock()
	}
	if hookHandle != 0 {
		return fmt.Errorf("hook did not stop in time")
	}
	return nil
}

// exitHookThread tears the pump thread down (GUI shutdown). Safe to
// call when the hook was never started.
func exitHookThread() {
	hookMu.Lock()
	hwnd := hookThreadHwnd
	hookMu.Unlock()
	if hwnd != 0 {
		procPostMessageW.Call(hwnd, hookExitMsg, 0, 0)
	}
}

// ---------------------------------------------------------------------------
// Hook thread
// ---------------------------------------------------------------------------

// user32/kernel32 calls via LazySystemDLL (no cgo, no duplicate win32
// binding dependency; launch_windows.go set the precedent).
var (
	user32                  = windows.NewLazySystemDLL("user32.dll")
	procSetWindowsHookExW   = user32.NewProc("SetWindowsHookExW")
	procUnhookWindowsHookEx = user32.NewProc("UnhookWindowsHookEx")
	procCallNextHookEx      = user32.NewProc("CallNextHookEx")
	procGetMessageW         = user32.NewProc("GetMessageW")
	procPostQuitMessage     = user32.NewProc("PostQuitMessage")
	procRegisterClassW      = user32.NewProc("RegisterClassW")
	procCreateWindowExW     = user32.NewProc("CreateWindowExW")
	procDefWindowProcW      = user32.NewProc("DefWindowProcW")
	procGetAsyncKeyState    = user32.NewProc("GetAsyncKeyState")
	procPostMessageW        = user32.NewProc("PostMessageW")
	kernel32                = windows.NewLazySystemDLL("kernel32.dll")
	procGetModuleHandleW    = kernel32.NewProc("GetModuleHandleW")
)

// Messages and flag bits not exported by golang.org/x/sys/windows.
const (
	wmKeydown    = 0x0100
	wmKeyup      = 0x0101
	wmSyskeydown = 0x0104
	wmSyskeyup   = 0x0105

	hookStopMsg = 0x8001 // WM_APP+1: end capture (un-hook)
	hookExitMsg = 0x8002 // WM_APP+2: tear the thread down

	llkhfExtended = 0x01 // KBDLLHOOKSTRUCT.flags: extended key
	llkhfLowerIl  = 0x02 // injected by a lower-integrity process
	llkhfInjected = 0x10 // injected event

	whKeyboardLL = 13
)

// kbdllHookStruct is the KBDLLHOOKSTRUCT the hook's lparam points at.
type kbdllHookStruct struct {
	vkCode    uint32
	scanCode  uint32
	flags     uint32
	time      uint32
	extraInfo uintptr
}

// Virtual-key codes of the modifiers the recorder tracks.
const (
	vkShift   = 0x10
	vkControl = 0x11
	vkMenu    = 0x12
	vkLWin    = 0x5B
	vkRWin    = 0x5C
)

var (
	// hookMu guards the hook thread's identity (handle, window). The
	// hook proc reads nothing through it — it snapshots the session
	// through keyCapture.mu — so a held hookMu never delays keystrokes.
	hookMu         sync.Mutex
	hookHandle     uintptr
	hookThreadHwnd uintptr
	hookClassOnce  sync.Once
	hookClassErr   error
	// spawnMu serializes hook-thread startup: two concurrent role starts
	// must not both see hookHandle == 0 and race two pump threads.
	spawnMu sync.Mutex
)

// ensureHook installs the low-level keyboard hook, spawning its pump
// thread. Idempotent: the hook stays installed between recordings so
// starting a session never races the install.
func ensureHook() error {
	spawnMu.Lock()
	defer spawnMu.Unlock()
	hookMu.Lock()
	if hookHandle != 0 {
		hookMu.Unlock()
		return nil
	}
	hookMu.Unlock()
	ready := make(chan error, 1)
	go func() { ready <- runHookThread() }()
	return <-ready
}

// runHookThread creates the message-only window, installs the hook,
// and pumps until the exit control arrives.
func runHookThread() error {
	hookClassOnce.Do(func() { hookClassErr = registerHookClass() })
	if hookClassErr != nil {
		return hookClassErr
	}
	hwnd := createHookWindow()
	if hwnd == 0 {
		return fmt.Errorf("CreateWindowExW failed")
	}
	hookMu.Lock()
	hookThreadHwnd = hwnd
	hookMu.Unlock()

	h, _, _ := procSetWindowsHookExW.Call(
		whKeyboardLL,
		windows.NewCallback(keyboardHookProc),
		0, // current module (hook procs in the same binary are fine)
		0, // all threads / system-wide
	)
	if h == 0 {
		hookMu.Lock()
		hookThreadHwnd = 0
		hookMu.Unlock()
		return fmt.Errorf("SetWindowsHookExW failed")
	}
	hookMu.Lock()
	hookHandle = h
	hookMu.Unlock()

	// The pump. GetMessageW returns 0 on WM_QUIT. Control messages are
	// handled in hookWndProc via dispatch; everything else is default-
	// proced (nothing else is sent to this window).
	var msg [6]uintptr // MSG layout: hwnd, message, wParam, lParam, time, pt
	for {
		r, _, _ := procGetMessageW.Call(uintptr(unsafe.Pointer(&msg[0])), 0, 0, 0)
		if r == 0 {
			break
		}
		m := (*msgLayout)(unsafe.Pointer(&msg[0]))
		switch m.message {
		case hookStopMsg, hookExitMsg:
			hookWndProc(m.hwnd, m.message, m.wParam, m.lParam)
		default:
			procDefWindowProcW.Call(m.hwnd, m.message, m.wParam, m.lParam)
		}
	}

	// Thread death: the hook may already be gone (stop control).
	hookMu.Lock()
	if hookHandle != 0 {
		procUnhookWindowsHookEx.Call(hookHandle)
		hookHandle = 0
	}
	hookThreadHwnd = 0
	hookMu.Unlock()
	return nil
}

// msgLayout mirrors MSG on 64-bit (32-bit pt fields at the tail).
type msgLayout struct {
	hwnd    uintptr
	message uintptr
	wParam  uintptr
	lParam  uintptr
	time    uint32
	ptX     int32
	ptY     int32
}

func registerHookClass() error {
	className, err := windows.UTF16PtrFromString("kvmshare-keycapture")
	if err != nil {
		return err
	}
	var wc struct {
		style         uint32
		lpfnWndProc   uintptr
		cbClsExtra    int32
		cbWndExtra    int32
		hInstance     uintptr
		hIcon         uintptr
		hCursor       uintptr
		hbrBackground uintptr
		lpszMenuName  uintptr
		lpszClassName uintptr
	}
	wc.lpfnWndProc = windows.NewCallback(hookWndProc)
	wc.lpszClassName = uintptr(unsafe.Pointer(className))
	hInstance, _, _ := procGetModuleHandleW.Call(0)
	wc.hInstance = hInstance
	atom, _, _ := procRegisterClassW.Call(uintptr(unsafe.Pointer(&wc)))
	if atom == 0 {
		return fmt.Errorf("RegisterClassW failed")
	}
	return nil
}

func createHookWindow() uintptr {
	className, _ := windows.UTF16PtrFromString("kvmshare-keycapture")
	const hwndMessage = ^uintptr(2) // HWND_MESSAGE = (HWND)-3
	h, _, _ := procCreateWindowExW.Call(
		0,
		uintptr(unsafe.Pointer(className)),
		0,          // no title
		0,          // no style
		0, 0, 0, 0, // no geometry
		hwndMessage, // parent: message-only
		0,           // no menu
		0,           // instance (from the class)
		0,           // no creation data
	)
	return h
}

// repeatTracker distinguishes auto-repeat from real re-presses: the
// low-level struct has no repeat flag, so consecutive downs of one VK
// with no intervening up are repeats.
type repeatTracker struct {
	mu   sync.Mutex
	vk   uint32
	down bool
}

var repeats repeatTracker

func isRepeat(vk uint32, down bool) bool {
	repeats.mu.Lock()
	defer repeats.mu.Unlock()
	if !down {
		if repeats.vk == vk {
			repeats.down = false
		}
		return false
	}
	if repeats.down && repeats.vk == vk {
		return true
	}
	repeats.down = true
	repeats.vk = vk
	return false
}

// keyboardHookProc is the WH_KEYBOARD_LL callback: invoked synchronously
// on the hook thread for every keyboard event system-wide, before the
// OS decides what the chord means.
func keyboardHookProc(code int32, wparam, lparam uintptr) uintptr {
	if code >= 0 {
		info := (*kbdllHookStruct)(unsafe.Pointer(lparam))
		msg := uint32(wparam)
		// Never touch injected events: the role binaries' SendInput and
		// any remote-control tool's synthetic input must pass through
		// untouched, or the shared session itself would break.
		if info.flags&(llkhfInjected|llkhfLowerIl) == 0 {
			down := msg == wmKeydown || msg == wmSyskeydown
			up := msg == wmKeyup || msg == wmSyskeyup
			if down || up {
					// The session snapshot is read fresh per event: the GUI
					// thread may swap or clear it at any moment.
					keyCapture.mu.Lock()
					sess := keyCapture.session
					expired := false
					if sess != nil && time.Now().After(sess.expires) {
						// TTL lapse: the page crashed or stalled. Kill the
						// session — suppression ends with it — tell the page
						// once, and let this event pass (the user regains a
						// live machine immediately).
						keyCapture.session = nil
						sess = nil
						expired = true
					}
					token := ""
					if sess != nil {
						token = sess.token
					}
					app := keyCapture.app
					keyCapture.mu.Unlock()

					if expired && app != nil {
						app.Emit(captureExpiredEvent, keyEvent{Token: token})
					}

					if sess != nil && app != nil {
						downNow := down
						repeat := isRepeat(info.vkCode, downNow)
						app.Emit("keycapture:"+token, keyEvent{
							Token:    token,
							Down:     downNow,
							Repeat:   repeat,
							VK:       info.vkCode,
							Scan:     info.scanCode,
							Extended: info.flags&llkhfExtended != 0,
							Control:  asyncDown(vkControl),
							Alt:      asyncDown(vkMenu),
							Shift:    asyncDown(vkShift),
							Meta:     asyncDown(vkLWin) || asyncDown(vkRWin),
						})
					// Suppression: the recorder owns the keyboard.
					// Returning without CallNextHookEx stops the event
					// dead — the shell never sees Super, the task
					// switcher never sees Super+Tab.
					return 1
				}
			}
		}
	}
	r, _, _ := procCallNextHookEx.Call(0, uintptr(code), wparam, lparam)
	return r
}

// asyncDown reads the live state of one virtual key.
func asyncDown(vk uint32) bool {
	r, _, _ := procGetAsyncKeyState.Call(uintptr(vk))
	return r&0x8000 != 0
}

// hookWndProc handles the control messages on the hook thread.
func hookWndProc(hwnd, message, wparam, lparam uintptr) uintptr {
	switch message {
	case hookStopMsg, hookExitMsg:
		hookMu.Lock()
		if hookHandle != 0 {
			procUnhookWindowsHookEx.Call(hookHandle)
			hookHandle = 0
		}
		hookMu.Unlock()
		if message == hookExitMsg {
			procPostQuitMessage.Call(0)
		}
		return 0
	}
	r, _, _ := procDefWindowProcW.Call(hwnd, message, wparam, lparam)
	return r
}


