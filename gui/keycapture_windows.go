//go:build windows

package main

// keycapture_windows.go — the OS-boundary keyboard hook behind shortcut
// recording.
//
// ## Why the webview's preventDefault is not enough
//
// The chord recorder runs inside the webview (WebView2). preventDefault
// there stops the *page* from acting on a key — it cannot stop the OS.
// Super+Tab's Tab never reaches the page at all: the shell consumes the
// chord before the window sees it. Recording a chord the OS has bound
// requires seeing the keys **before the OS does** and swallowing them
// there — the same boundary the role binaries' shortcut engine already
// uses (crates/platform/src/windows/capture.rs).
//
// ## The lifecycle contract
//
// The hook installs once, in the background, on first use, and stays
// installed: a pump thread owns it (low-level hooks require a pumping
// thread), a message-only window receives control messages on that
// pump, and the hook callback consults the captureRegistry per event.
//
// The bridge calls never block on the hook thread and the hook never
// blocks on the GUI:
//
//   - StartKeyCapture arms the session in the registry and returns
//     immediately; hook installation proceeds in the background.
//   - Session changes reach the pump through a buffered command
//     channel; the callback reads the registry's current state per
//     event, so a decision is never stale.
//   - Every event the session covers is emitted to the page through
//     the registry's sink (Wails' mailbox-backed emit — non-blocking)
//     and suppressed by returning 1 from the callback.
//   - Injected events (the role binaries' SendInput, remote-control
//     tools) are never touched: LLKHF_INJECTED / LLKHF_LOWER_IL_
//     INJECTED pass through untouched, or the shared session itself
//     would break.
//   - A lapsed TTL ends the session on the next key (per-key path) or
//     the watchdog (no keys at all); the page is told once and keys
//     pass through — a crashed page can never leave the machine
//     silent.

import (
	"fmt"
	"sync"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
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

// Virtual-key codes of the modifiers the recorder tracks. Low-level
// hooks report the L/R-specific virtual keys (VK_LCONTROL 0xA2 …), so
// each field ORs its pair.
const (
	vkShift    = 0x10
	vkControl  = 0x11
	vkMenu     = 0x12
	vkLWin     = 0x5B
	vkRWin     = 0x5C
	vkLControl = 0xA2
	vkRControl = 0xA3
	vkLShift   = 0xA0
	vkRShift   = 0xA1
	vkLMenu    = 0xA4
	vkRMenu    = 0xA5
)

// modState tracks the live modifier set from the event stream itself.
//
// GetAsyncKeyState is deliberately NOT used: the hook swallows every
// event while a session is live, and a swallowed event never updates
// the system async-key table — the snapshot would read "not pressed"
// for the very Super the user is holding, recorded chords would lose
// their modifiers, and validation would answer "Add a modifier" for a
// chord that plainly had one (measured, reproducible).
type modState struct {
	mu             sync.Mutex
	lCtrl, rCtrl   bool
	lShift, rShift bool
	lAlt, rAlt     bool
	lWin, rWin     bool
}

var mods modState

// apply folds one hook event into the tracked modifier set (call for
// every down/up of every key, before any suppression decision).
func (m *modState) apply(vk uint32, down bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	switch vk {
	case vkControl, vkLControl, vkRControl:
		if vk == vkRControl {
			m.rCtrl = down
		} else {
			m.lCtrl = down
		}
	case vkShift, vkLShift, vkRShift:
		if vk == vkRShift {
			m.rShift = down
		} else {
			m.lShift = down
		}
	case vkMenu, vkLMenu, vkRMenu:
		if vk == vkRMenu {
			m.rAlt = down
		} else {
			m.lAlt = down
		}
	case vkLWin:
		m.lWin = down
	case vkRWin:
		m.rWin = down
	}
}

// snapshot reads the tracked set.
func (m *modState) snapshot() (ctrl, alt, shift, meta bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.lCtrl || m.rCtrl, m.lAlt || m.rAlt, m.lShift || m.rShift, m.lWin || m.rWin
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

// hookCmd is a control message for the pump thread.
type hookCmd int

const (
	hookCmdStop hookCmd = iota // un-hook (no session live)
	hookCmdExit                // tear the thread down (GUI shutdown)
)

var (
	// hookMu guards the hook thread's identity (handle, window, cmd
	// channel). The hook proc reads nothing through it — it consults
	// the captureRegistry — so a held hookMu never delays keystrokes.
	hookMu         sync.Mutex
	hookHandle     uintptr
	hookThreadHwnd uintptr
	hookCmds       chan hookCmd
	hookClassOnce  sync.Once
	hookClassErr   error

	user32                  = windows.NewLazySystemDLL("user32.dll")
	procSetWindowsHookExW   = user32.NewProc("SetWindowsHookExW")
	procUnhookWindowsHookEx = user32.NewProc("UnhookWindowsHookEx")
	procCallNextHookEx      = user32.NewProc("CallNextHookEx")
	procGetMessageW         = user32.NewProc("GetMessageW")
	procPostQuitMessage     = user32.NewProc("PostQuitMessage")
	procRegisterClassW      = user32.NewProc("RegisterClassW")
	procCreateWindowExW     = user32.NewProc("CreateWindowExW")
	procDefWindowProcW      = user32.NewProc("DefWindowProcW")
	procPostMessageW        = user32.NewProc("PostMessageW")
	kernel32                = windows.NewLazySystemDLL("kernel32.dll")
	procGetModuleHandleW    = kernel32.NewProc("GetModuleHandleW")
)

// ensureHook installs the low-level keyboard hook once, spawning its
// pump thread in the background. It never blocks the caller on hook
// startup: the first keystroke of a recording may arrive a few
// milliseconds before the install completes, and the registry simply
// reports nothing until the callback exists — a session that is armed
// but not yet reporting does not suppress anything (decideFor only
// suppresses from the callback, which by definition only runs once
// installed).
func ensureHook() {
	hookMu.Lock()
	if hookHandle != 0 || hookCmds != nil {
		// Installed, or a pump is starting. Either way the background
		// path owns the rest.
		hookMu.Unlock()
		return
	}
	hookCmds = make(chan hookCmd, 8)
	hookMu.Unlock()
	go runHookThread()
}

// runHookThread creates the message-only window, installs the hook,
// and pumps until the exit control arrives. It signals the registry
// about install success/failure and exits when the hook is gone.
func runHookThread() {
	hookClassOnce.Do(func() { hookClassErr = registerHookClass() })
	if hookClassErr != nil {
		keyCapture.markHookInstalled(false)
		return
	}
	hwnd := createHookWindow()
	if hwnd == 0 {
		keyCapture.markHookInstalled(false)
		return
	}
	h, _, _ := procSetWindowsHookExW.Call(
		whKeyboardLL,
		windows.NewCallback(keyboardHookProc),
		0, // current module (hook procs in the same binary are fine)
		0, // all threads / system-wide
	)
	if h == 0 {
		keyCapture.markHookInstalled(false)
		return
	}
	hookMu.Lock()
	hookHandle = h
	hookThreadHwnd = hwnd
	hookMu.Unlock()
	keyCapture.markHookInstalled(true)

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
		// Session changes wake the pump through the channel so a
		// stop is applied promptly even with no keystroke in flight.
		drainHookCmds()
	}

	// Thread death: the hook may already be gone (stop control).
	hookMu.Lock()
	if hookHandle != 0 {
		procUnhookWindowsHookEx.Call(hookHandle)
		hookHandle = 0
	}
	hookThreadHwnd = 0
	hookCmds = nil
	hookMu.Unlock()
	keyCapture.markHookInstalled(false)
}

// drainHookCmds applies pending stop/exit commands. Called from the
// pump loop; the callback never runs it.
func drainHookCmds() {
	for {
		hookMu.Lock()
		cmds := hookCmds
		hookMu.Unlock()
		if cmds == nil {
			return
		}
		select {
		case cmd := <-cmds:
			switch cmd {
			case hookCmdStop:
				hookMu.Lock()
				if hookHandle != 0 {
					procUnhookWindowsHookEx.Call(hookHandle)
					hookHandle = 0
				}
				hookMu.Unlock()
				keyCapture.markHookInstalled(false)
			case hookCmdExit:
				hookMu.Lock()
				if hookHandle != 0 {
					procUnhookWindowsHookEx.Call(hookHandle)
					hookHandle = 0
				}
				hwnd := hookThreadHwnd
				hookMu.Unlock()
				keyCapture.markHookInstalled(false)
				if hwnd != 0 {
					procPostMessageW.Call(hwnd, hookExitMsg, 0, 0)
				}
				return
			}
		default:
			return
		}
	}
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

// StartKeyCapture begins a capture session. Returns the event name the
// page subscribes to. Hook installation proceeds in the background;
// the returned session is armed the moment this call returns, so the
// page can subscribe and receive events as soon as the hook exists.
// Bound for the frontend.
func (a *App) StartKeyCapture(token string) (string, error) {
	a.mu.Lock()
	em := a.events
	a.mu.Unlock()
	if err := keyCapture.arm(token, em); err != nil {
		return "", err
	}
	ensureHook() // background install; never blocks the caller
	return "keycapture:" + token, nil
}

// StopKeyCapture ends the session with this token (a stale token is a
// no-op, not an error). Bound for the frontend.
func (a *App) StopKeyCapture(token string) error {
	keyCapture.disarm(token)
	return nil
}

// RenewKeyCapture extends the session (the recorder renews on every
// received key, so a long thoughtful pause then a key still works).
// Bound for the frontend.
func (a *App) RenewKeyCapture(token string) error {
	keyCapture.renew(token)
	return nil
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

// keyboardHookProc is the WH_KEYBOARD_LL callback: invoked synchronously
// on the hook thread for every keyboard event system-wide, before the
// OS decides what the chord means.
//
// go vet's unsafeptr check flags the lparam conversion below as a
// "possible misuse"; it is not. The pointer originates in Windows —
// the OS hands the hook callback a KBDLLHOOKSTRUCT* as lparam — and is
// never derived from a Go pointer, so there is no roundtrip to misuse.
// This is the canonical form every Go low-level-hook implementation
// uses.
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
				// Track the modifier set from the stream itself, before
				// anything else: the async-key table cannot see events
				// we swallow (see modState docs).
				mods.apply(info.vkCode, down)

				token, suppress, sink, expired := keyCapture.decideFor(time.Now())
				if expired && sink != nil {
					sink.Emit(captureExpiredEvent, keyEvent{Token: token})
				}
				if suppress && sink != nil {
					repeat := isRepeat(info.vkCode, down)
					ctrl, alt, shift, meta := mods.snapshot()
					sink.Emit("keycapture:"+token, keyEvent{
						Token:    token,
						Down:     down,
						Repeat:   repeat,
						VK:       info.vkCode,
						Scan:     info.scanCode,
						Extended: info.flags&llkhfExtended != 0,
						Control:  ctrl,
						Alt:      alt,
						Shift:    shift,
						Meta:     meta,
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

// hookWndProc handles the control messages on the pump thread.
func hookWndProc(hwnd, message, wparam, lparam uintptr) uintptr {
	switch message {
	case hookStopMsg:
		hookMu.Lock()
		if hookHandle != 0 {
			procUnhookWindowsHookEx.Call(hookHandle)
			hookHandle = 0
		}
		hookMu.Unlock()
		keyCapture.markHookInstalled(false)
		return 0
	case hookExitMsg:
		hookMu.Lock()
		if hookHandle != 0 {
			procUnhookWindowsHookEx.Call(hookHandle)
			hookHandle = 0
		}
		hookMu.Unlock()
		keyCapture.markHookInstalled(false)
		procPostQuitMessage.Call(0)
		return 0
	}
	r, _, _ := procDefWindowProcW.Call(hwnd, message, wparam, lparam)
	return r
}

// The registry's wakeHook: nudge the pump so a session change is
// applied even when no keystroke is in flight. Posting to the
// message-only window is the cheapest safe nudge (the pump's drain
// runs after every message).
func wakeHookPump() {
	hookMu.Lock()
	hwnd := hookThreadHwnd
	hookMu.Unlock()
	if hwnd != 0 {
		procPostMessageW.Call(hwnd, hookStopMsg+100, 0, 0) // WM_APP+101: no-op nudge
	}
}

// exitHookThread tears the pump thread down (GUI shutdown). Safe to
// call when the hook was never started.
func exitHookThread() {
	hookMu.Lock()
	cmds := hookCmds
	hookMu.Unlock()
	if cmds != nil {
		select {
		case cmds <- hookCmdExit:
		default:
		}
	}
	hookMu.Lock()
	hwnd := hookThreadHwnd
	hookMu.Unlock()
	if hwnd != 0 {
		procPostMessageW.Call(hwnd, hookExitMsg, 0, 0)
	}
}
