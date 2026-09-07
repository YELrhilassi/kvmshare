//go:build linux

package main

// XEmbed system-tray fallback, implemented directly on Xlib.
//
// Wails exposes the tray over SNI (org.kde.StatusNotifierItem), which only
// GNOME/KDE/waybar-style hosts pick up. Bars like i3bar, xfce4-panel,
// trayer and stalonetray speak the older XEmbed protocol instead. When no
// SNI watcher exists, kvmshare embeds a native X11 tray icon itself —
// plug-and-play on any X11 desktop, no external bridge packages.
//
// GTK is deliberately NOT used here: the Wails app links GTK4, and pulling
// GTK3 in through cgo (for GtkStatusIcon) collides two GType systems in
// one process ("cannot register existing type 'GdkDisplay'"). Raw Xlib is
// already linked and has no such conflict.
//
// One dedicated C thread owns the X connection for the icon's lifetime:
// it creates the window, docks into the tray manager, redraws on Expose,
// re-docks if the manager restarts, and reports clicks back to Go. All
// tray state lives behind that thread — no locking needed.
//
// XEmbed has no menu protocol, so the icon's right-click opens a small
// popup menu implemented directly on Xlib (a grab + an override-redirect
// window). The menu's labels are rebuilt from Go state every time it
// opens, and its actions call back into Go via //export functions.

/*
#cgo pkg-config: x11
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/Xatom.h>
#include <X11/cursorfont.h>
#include <X11/keysym.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/select.h>
#include <sys/time.h>

extern void kvmTrayOpen(void);
extern void kvmTrayStart(void);
extern void kvmTrayStop(void);
extern void kvmTrayRestart(void);
extern void kvmTrayQuit(void);
extern char *kvmTrayState(void);

static Display *tray_dpy = NULL;
static Window   tray_win = 0;
static Window   tray_manager = 0;
static unsigned char *tray_src = NULL;   // original RGBA icon
static int      tray_src_w = 0, tray_src_h = 0;
static char     tray_sel[64];
static int      stop_pipe[2] = {-1, -1};

// tray_ignore_error swallows X errors on the tray connection. The icon
// thread races the manager (it may destroy our window mid-redraw) and no
// such error is fatal for the tray's purpose — the default Xlib handler
// would print and keep going, but a dedicated handler makes it explicit
// that the tray must never crash the GUI.
static int tray_ignore_error(Display *d, XErrorEvent *e) { return 0; }

// shift_of returns how far a channel mask sits from the LSB.
static int shift_of(unsigned long mask) {
	int s = 0;
	while (mask && !(mask & 1)) { mask >>= 1; s++; }
	return s;
}

// scale_rgba downsamples an RGBA image from sw x sh to dw x dh with box
// averaging — the right filter when shrinking (a 64px icon into a ~19px
// tray slot): each output pixel is the average of its source region, so
// edges stay clean instead of aliasing.
static unsigned char *scale_rgba(const unsigned char *src, int sw, int sh,
	int dw, int dh) {
	unsigned char *out = malloc((size_t)dw * dh * 4);
	if (out == NULL) return NULL;
	for (int y = 0; y < dh; y++) {
		int ys0 = (int)((long long)y * sh / dh);
		int ys1 = (int)((long long)(y + 1) * sh / dh);
		if (ys1 <= ys0) ys1 = ys0 + 1;
		for (int x = 0; x < dw; x++) {
			int xs0 = (int)((long long)x * sw / dw);
			int xs1 = (int)((long long)(x + 1) * sw / dw);
			if (xs1 <= xs0) xs1 = xs0 + 1;
			unsigned r = 0, g = 0, b = 0, a = 0;
			for (int sy = ys0; sy < ys1; sy++) {
				for (int sx = xs0; sx < xs1; sx++) {
					const unsigned char *p = src + ((size_t)sy * sw + sx) * 4;
					r += p[0]; g += p[1]; b += p[2]; a += p[3];
				}
			}
			unsigned n = (unsigned)(ys1 - ys0) * (unsigned)(xs1 - xs0);
			unsigned char *d = out + ((size_t)y * dw + x) * 4;
			d[0] = (unsigned char)(r / n);
			d[1] = (unsigned char)(g / n);
			d[2] = (unsigned char)(b / n);
			d[3] = (unsigned char)(a / n);
		}
	}
	return out;
}

// make_icon builds a ZPixmap XImage of size dw x dh from RGBA bytes,
// compositing over the app's dark background so the icon reads cleanly
// on any bar.
static XImage *make_icon(const unsigned char *rgba, int dw, int dh) {
	Visual *vis = DefaultVisual(tray_dpy, DefaultScreen(tray_dpy));
	int depth = DefaultDepth(tray_dpy, DefaultScreen(tray_dpy));
	char *buf = malloc((size_t)dw * dh * 4);
	if (buf == NULL) return NULL;
	XImage *img = XCreateImage(tray_dpy, vis, depth, ZPixmap, 0, buf, dw, dh, 32, 0);
	if (img == NULL) { free(buf); return NULL; }
	unsigned long rm = vis->red_mask, gm = vis->green_mask, bm = vis->blue_mask;
	int rs = shift_of(rm), gs = shift_of(gm), bs = shift_of(bm);
	for (int y = 0; y < dh; y++) {
		for (int x = 0; x < dw; x++) {
			const unsigned char *p = rgba + ((size_t)y * dw + x) * 4;
			// Composite over #0a0a0c.
			unsigned r = (p[0] * p[3] + 10 * (255 - p[3])) / 255;
			unsigned g = (p[1] * p[3] + 10 * (255 - p[3])) / 255;
			unsigned b = (p[2] * p[3] + 12 * (255 - p[3])) / 255;
			unsigned long px = ((r << rs) & rm) | ((g << gs) & gm) | ((b << bs) & bm);
			XPutPixel(img, x, y, px);
		}
	}
	return img;
}

// tray_draw redraws the icon at the window's CURRENT size: the tray
// manager resizes the embedded window to its own slot size (e.g. 19px),
// so the source icon is scaled to fit rather than cropped.
static void tray_draw(void) {
	if (tray_dpy == NULL || tray_win == 0 || tray_src == NULL) return;
	Window root;
	int x, y;
	unsigned w, h, border, depth;
	if (XGetGeometry(tray_dpy, tray_win, &root, &x, &y, &w, &h, &border, &depth) == 0)
		return;
	if (w == 0 || h == 0) return;
	unsigned char *scaled = scale_rgba(tray_src, tray_src_w, tray_src_h, (int)w, (int)h);
	if (scaled == NULL) return;
	XImage *img = make_icon(scaled, (int)w, (int)h);
	free(scaled);
	if (img == NULL) return;
	GC gc = DefaultGC(tray_dpy, DefaultScreen(tray_dpy));
	XPutImage(tray_dpy, tray_win, gc, img, 0, 0, 0, 0, w, h);
	XDestroyImage(img);
	XFlush(tray_dpy);
}

// tray_create makes the icon window and announces it via _XEMBED_INFO.
// The window is deliberately NOT mapped here: per the freedesktop
// system-tray spec the client must stay unmapped until the tray manager
// reparents and maps it (the XEMBED_MAPPED flag in _XEMBED_INFO is what
// asks the manager to map it). Mapping early makes the icon float as a
// standalone window on the desktop — the WM even manages it as a normal
// window.
static void tray_create(int w, int h) {
	int scr = DefaultScreen(tray_dpy);
	tray_win = XCreateSimpleWindow(tray_dpy, RootWindow(tray_dpy, scr),
		0, 0, w, h, 0, 0, BlackPixel(tray_dpy, scr));
	long info[2] = {0, 1}; // protocol version 0, XEMBED_MAPPED
	Atom xembed = XInternAtom(tray_dpy, "_XEMBED_INFO", False);
	XChangeProperty(tray_dpy, tray_win, xembed, xembed, 32, PropModeReplace,
		(unsigned char *)info, 2);
	XSelectInput(tray_dpy, tray_win, ExposureMask | ButtonPressMask | StructureNotifyMask);
	XFlush(tray_dpy);
}

// tray_timestamp returns a real server timestamp for the dock request.
// Tray managers reject a dock sent with CurrentTime (0); the request must
// carry a real timestamp. With no user event to reuse at startup, the
// standard way to obtain one is to change a property on our own window
// and read the time the server stamps on the resulting PropertyNotify.
// XSync forces the round trip, so the event is guaranteed to be queued
// before XCheckWindowEvent looks for it — no race, no hang.
static unsigned long tray_timestamp(void) {
	// Watch for the property change we are about to make.
	XSelectInput(tray_dpy, tray_win, PropertyChangeMask | ExposureMask |
		ButtonPressMask | StructureNotifyMask);
	Atom xembed = XInternAtom(tray_dpy, "_XEMBED_INFO", False);
	long info[2] = {0, 1};
	XChangeProperty(tray_dpy, tray_win, xembed, xembed, 32, PropModeReplace,
		(unsigned char *)info, 2);
	// The server processes the request in order and delivers the
	// PropertyNotify before the XSync reply; after this returns the
	// event is in our queue.
	XSync(tray_dpy, False);

	unsigned long ts = CurrentTime;
	XEvent ev;
	if (XCheckWindowEvent(tray_dpy, tray_win, PropertyChangeMask, &ev) != 0) {
		ts = ev.xproperty.time;
	}
	// Restore the event mask the icon advertises.
	XSelectInput(tray_dpy, tray_win, ExposureMask | ButtonPressMask | StructureNotifyMask);
	return ts;
}

// tray_dock sends the SYSTEM_TRAY_REQUEST_DOCK client message to the
// manager that owns the _NET_SYSTEM_TRAY_S<n> selection.
//
// Per the freedesktop system-tray spec the message_type must be
// _NET_SYSTEM_TRAY_OPCODE (managers test that atom and ignore anything
// else), data.l[0] a REAL server timestamp (CurrentTime is rejected),
// data.l[1] the opcode 0 (SYSTEM_TRAY_REQUEST_DOCK) and data.l[2] our
// window.
static void tray_dock(void) {
	Atom sel = XInternAtom(tray_dpy, tray_sel, False);
	Window mgr = XGetSelectionOwner(tray_dpy, sel);
	if (mgr == None) { tray_manager = 0; return; }
	tray_manager = mgr;
	// Watch the manager window itself so we notice immediately when it
	// dies (our embedded window is then reparented to the root and would
	// otherwise float on the desktop until the next poll).
	XSelectInput(tray_dpy, mgr, StructureNotifyMask);
	unsigned long ts = tray_timestamp();
	XEvent ev;
	memset(&ev, 0, sizeof ev);
	ev.xclient.type = ClientMessage;
	ev.xclient.window = mgr;
	ev.xclient.message_type = XInternAtom(tray_dpy, "_NET_SYSTEM_TRAY_OPCODE", False);
	ev.xclient.format = 32;
	ev.xclient.data.l[0] = (long)ts;
	ev.xclient.data.l[1] = 0;                    // SYSTEM_TRAY_REQUEST_DOCK
	ev.xclient.data.l[2] = (long)tray_win;       // our window
	XSendEvent(tray_dpy, mgr, False, NoEventMask, &ev);
	XFlush(tray_dpy);
}

// xembed_manager_present reports whether a tray manager owns the
// selection right now. Opens its own short-lived display connection.
static int xembed_manager_present(void) {
	Display *d = XOpenDisplay(NULL);
	if (d == NULL) return 0;
	char sel[64];
	snprintf(sel, sizeof sel, "_NET_SYSTEM_TRAY_S%d", DefaultScreen(d));
	Window owner = XGetSelectionOwner(d, XInternAtom(d, sel, False));
	XCloseDisplay(d);
	return owner != None;
}

// ---------------------------------------------------------------------------
// Popup menu. XEmbed has no menu protocol, so the icon's right-click opens
// this small override-redirect window with a pointer+keyboard grab. It is
// transient by design: built per click, destroyed on selection or dismiss.
// ---------------------------------------------------------------------------

#define MENU_W 190
#define MENU_ITEM_H 26
#define MENU_PAD 4
#define MENU_ITEMS 5

static Window menu_win = 0;
static int    menu_running = 0;      // role running? (from kvmTrayState)
static char   menu_state[64];        // "server · running" row
static char   menu_startstop[64];    // "Start server" / "Stop server"

static int menu_height(void) { return MENU_PAD * 2 + MENU_ITEMS * MENU_ITEM_H; }

// menu_refresh_labels pulls the current role state from Go and rebuilds
// the dynamic menu text. Called every time the menu opens, so the labels
// are always current.
static void menu_refresh_labels(void) {
	char *st = kvmTrayState();
	if (st != NULL) {
		char *sep = strrchr(st, '|');
		if (sep != NULL) {
			menu_running = (sep[1] == '1');
			*sep = '\0';
		}
		snprintf(menu_state, sizeof menu_state, "%s · %s", st,
			menu_running ? "running" : "stopped");
		snprintf(menu_startstop, sizeof menu_startstop, "%s %s",
			menu_running ? "Stop" : "Start", st);
		free(st);
	} else {
		snprintf(menu_state, sizeof menu_state, "kvmshare");
		snprintf(menu_startstop, sizeof menu_startstop, "Start");
	}
}

// menu_item_text returns the label for row i (NULL for none).
static const char *menu_item_text(int i) {
	switch (i) {
	case 0: return menu_state;
	case 1: return "Open kvmshare";
	case 2: return menu_startstop;
	case 3: return "Restart";
	case 4: return "Quit";
	}
	return NULL;
}

// menu_draw paints the menu: dark surface, dimmed state row, a separator,
// and an accent highlight on the hovered item.
static void menu_draw(XFontStruct *fs, int hover) {
	if (menu_win == 0) return;
	GC gc = DefaultGC(tray_dpy, DefaultScreen(tray_dpy));
	int h = menu_height();
	XSetForeground(tray_dpy, gc, 0x1b1b1f);
	XFillRectangle(tray_dpy, menu_win, gc, 0, 0, MENU_W, h);
	// Separator under the state row.
	XSetForeground(tray_dpy, gc, 0x3a3a40);
	XFillRectangle(tray_dpy, menu_win, gc, 10, MENU_PAD + MENU_ITEM_H - 1, MENU_W - 20, 1);
	for (int i = 0; i < MENU_ITEMS; i++) {
		const char *text = menu_item_text(i);
		if (text == NULL) continue;
		int y0 = MENU_PAD + i * MENU_ITEM_H;
		if (i == hover) {
			XSetForeground(tray_dpy, gc, 0x2d6cf6);
			XFillRectangle(tray_dpy, menu_win, gc, 2, y0, MENU_W - 4, MENU_ITEM_H);
			XSetForeground(tray_dpy, gc, 0xffffff);
		} else if (i == 0) {
			XSetForeground(tray_dpy, gc, 0x8a8a90);
		} else {
			XSetForeground(tray_dpy, gc, 0xe8e8ea);
		}
		int ty = y0 + (MENU_ITEM_H + fs->ascent - fs->descent) / 2;
		XDrawString(tray_dpy, menu_win, gc, 12, ty, text, (int)strlen(text));
	}
	XFlush(tray_dpy);
}

// menu_item_at maps a position inside the menu window to a row, or 0 when
// the position is outside the menu or on the non-clickable state row.
static int menu_item_at(int x, int y) {
	if (x < 0 || y < 0 || x >= MENU_W || y >= menu_height()) return 0;
	int i = (y - MENU_PAD) / MENU_ITEM_H;
	if (i < 0 || i >= MENU_ITEMS) return 0;
	return i;
}

// menu_run shows the popup at the pointer, runs it until an item is
// chosen or it is dismissed, and returns the chosen row (0 = dismissed).
static int menu_run(int px, int py) {
	int scr = DefaultScreen(tray_dpy);
	int sw = WidthOfScreen(DefaultScreenOfDisplay(tray_dpy));
	int sh = HeightOfScreen(DefaultScreenOfDisplay(tray_dpy));
	int mw = MENU_W, mh = menu_height();
	if (px + mw > sw) px = sw - mw;
	if (py + mh > sh) py = sh - mh;
	if (px < 0) px = 0;
	if (py < 0) py = 0;

	menu_refresh_labels();

	menu_win = XCreateSimpleWindow(tray_dpy, RootWindow(tray_dpy, scr),
		px, py, mw, mh, 1, 0x4a4a50, 0x1b1b1f);
	XStoreName(tray_dpy, menu_win, "kvmshare");
	XSetWindowAttributes attrs;
	attrs.override_redirect = True;
	XChangeWindowAttributes(tray_dpy, menu_win, CWOverrideRedirect, &attrs);
	XSelectInput(tray_dpy, menu_win, ExposureMask | ButtonPressMask |
		ButtonReleaseMask | PointerMotionMask | KeyPressMask);
	XMapRaised(tray_dpy, menu_win);

	XFontStruct *fs = XLoadQueryFont(tray_dpy, "fixed");
	if (fs == NULL) {
		XDestroyWindow(tray_dpy, menu_win);
		menu_win = 0;
		return 0;
	}

	Cursor arrow = XCreateFontCursor(tray_dpy, XC_left_ptr);
	if (XGrabPointer(tray_dpy, menu_win, False,
		ButtonPressMask | ButtonReleaseMask | PointerMotionMask,
		GrabModeAsync, GrabModeAsync, None, arrow, CurrentTime) != GrabSuccess) {
		XFreeFont(tray_dpy, fs);
		XFreeCursor(tray_dpy, arrow);
		XDestroyWindow(tray_dpy, menu_win);
		menu_win = 0;
		return 0;
	}
	// A keyboard grab is best-effort: Escape-to-dismiss is nice but not
	// worth failing the menu over if another app holds the keyboard.
	XGrabKeyboard(tray_dpy, menu_win, False, GrabModeAsync, GrabModeAsync, CurrentTime);
	XFlush(tray_dpy);

	int hover = -1, armed = 0, action = 0, done = 0;
	menu_draw(fs, hover);

	while (!done) {
		XEvent ev;
		XNextEvent(tray_dpy, &ev);
		switch (ev.type) {
		case Expose:
			menu_draw(fs, hover);
			break;
		case MotionNotify: {
			int i = menu_item_at(ev.xmotion.x, ev.xmotion.y);
			if (i != hover) { hover = i; menu_draw(fs, hover); }
			break;
		}
		case ButtonPress: {
			int i = menu_item_at(ev.xbutton.x, ev.xbutton.y);
			if (i > 0) { armed = i; menu_draw(fs, hover); }
			else { done = 1; } // click outside the menu: dismiss
			break;
		}
		case ButtonRelease: {
			int i = menu_item_at(ev.xbutton.x, ev.xbutton.y);
			// Release on the armed item selects it; release anywhere else
			// dismisses (standard popup-menu behavior).
			if (i > 0 && i == armed) { action = i; }
			done = 1;
			armed = 0;
			break;
		}
		case KeyPress: {
			KeySym ks = XLookupKeysym(&ev.xkey, 0);
			if (ks == XK_Escape || ks == XK_Return) { done = 1; }
			break;
		}
		}
	}

	XUngrabKeyboard(tray_dpy, CurrentTime);
	XUngrabPointer(tray_dpy, CurrentTime);
	XDestroyWindow(tray_dpy, menu_win);
	menu_win = 0;
	XFreeCursor(tray_dpy, arrow);
	XFreeFont(tray_dpy, fs);
	XFlush(tray_dpy);
	return action;
}

// xembed_tray_run owns the icon for the process lifetime: init, then an
// event loop that redraws, re-docks when the manager restarts, opens the
// window on left-click, and shows the menu on right-click. Returns only
// when the stop pipe is written.
static int xembed_tray_run(const unsigned char *rgba, int w, int h) {
	tray_dpy = XOpenDisplay(NULL);
	if (tray_dpy == NULL) return 0;
	XSetErrorHandler(tray_ignore_error);
	snprintf(tray_sel, sizeof tray_sel, "_NET_SYSTEM_TRAY_S%d",
		DefaultScreen(tray_dpy));

	tray_src = malloc((size_t)w * h * 4);
	if (tray_src == NULL) { XCloseDisplay(tray_dpy); tray_dpy = NULL; return 0; }
	memcpy(tray_src, rgba, (size_t)w * h * 4);
	tray_src_w = w; tray_src_h = h;

	tray_create(w, h);
	tray_dock();

	int xfd = ConnectionNumber(tray_dpy);
	if (pipe(stop_pipe) != 0) {
		free(tray_src); tray_src = NULL;
		XCloseDisplay(tray_dpy); tray_dpy = NULL;
		return 0;
	}

	for (;;) {
		fd_set fds;
		FD_ZERO(&fds);
		FD_SET(xfd, &fds);
		FD_SET(stop_pipe[0], &fds);
		struct timeval tv = {5, 0}; // periodic manager re-check
		int ready = select((xfd > stop_pipe[0] ? xfd : stop_pipe[0]) + 1, &fds, NULL, NULL, &tv);
		if (ready < 0) continue;
		if (FD_ISSET(stop_pipe[0], &fds)) break;
		if (FD_ISSET(xfd, &fds)) {
			while (XPending(tray_dpy) > 0) {
				XEvent ev;
				XNextEvent(tray_dpy, &ev);
				switch (ev.type) {
				case Expose:
					tray_draw();
					break;
				case ButtonPress:
					if (ev.xbutton.button == Button1) {
						kvmTrayOpen();
					} else if (ev.xbutton.button == Button3) {
						int a = menu_run(ev.xbutton.x_root, ev.xbutton.y_root);
						switch (a) {
						case 1: kvmTrayOpen(); break;
						case 2: if (menu_running) kvmTrayStop(); else kvmTrayStart(); break;
						case 3: kvmTrayRestart(); break;
						case 4: kvmTrayQuit(); break;
						}
					}
					break;
				case DestroyNotify:
					if (ev.xdestroywindow.window == tray_win) {
						// Our window was destroyed (manager restart):
						// recreate on the next poll and re-dock.
						tray_win = 0;
						tray_manager = 0;
					} else if (ev.xdestroywindow.window == tray_manager) {
						// The manager died: our embedded window is
						// reparented to the root (still mapped) — hide
						// it so it never floats on the desktop.
						tray_manager = 0;
						if (tray_win != 0) XUnmapWindow(tray_dpy, tray_win);
					}
					break;
				}
			}
		} else {
			// Timeout: reconcile with the tray manager.
			Atom sel = XInternAtom(tray_dpy, tray_sel, False);
			Window mgr = XGetSelectionOwner(tray_dpy, sel);
			if (mgr == None) {
				// No manager right now. If we were embedded and it died
				// without a DestroyNotify reaching us (or before we
				// subscribed), our window sits mapped on the root — hide
				// it so it never floats, and dock again when one returns.
				if (tray_manager != 0) {
					tray_manager = 0;
					if (tray_win != 0) XUnmapWindow(tray_dpy, tray_win);
				}
				continue;
			}
			if (tray_win == 0) tray_create(tray_src_w, tray_src_h);
			if (mgr != tray_manager) tray_dock();
		}
	}

	if (tray_win != 0) XDestroyWindow(tray_dpy, tray_win);
	free(tray_src);
	tray_src = NULL;
	XCloseDisplay(tray_dpy);
	tray_dpy = NULL;
	close(stop_pipe[0]);
	close(stop_pipe[1]);
	stop_pipe[0] = stop_pipe[1] = -1;
	return 1;
}
*/
import "C"
import (
	"bytes"
	"image"
	"image/png"
	"sync"
	"time"

	"github.com/wailsapp/wails/v3/pkg/application"
)

// xembedManagerPresent reports whether an XEmbed tray manager exists.
// Pure Xlib — safe from any goroutine, no GTK involved.
func xembedManagerPresent() bool {
	return C.xembed_manager_present() != 0
}

// setupXEmbedTray embeds a native X11 tray icon and serves it until the
// process exits. Left-click opens the window; right-click shows the popup
// menu (Open, Start/Stop, Restart, Quit) whose labels track the live role
// state. `core` and `win` must outlive the app.
func setupXEmbedTray(app *application.App, core *App, win *application.WebviewWindow) {
	icon, err := decodeTrayIcon()
	if err != nil {
		app.Logger.Info("tray: XEmbed fallback unavailable — bad icon asset")
		return
	}

	xembedAct = xembedActions{
		open: func() {
			app.Logger.Info("tray: menu — open window")
			win.Show()
			win.Focus()
		},
		start: func() {
			app.Logger.Info("tray: menu — start role")
			if _, err := core.StartActive(); err != nil {
				app.Logger.Error("tray: menu — start failed", "err", err)
			}
		},
		stop: func() {
			app.Logger.Info("tray: menu — stop role")
			if err := core.StopActive(); err != nil {
				app.Logger.Error("tray: menu — stop failed", "err", err)
			}
		},
		restart: func() {
			// StopActive waits for the role lock to be released, so the
			// start that follows cannot collide with the old instance.
			app.Logger.Info("tray: menu — restart role")
			if err := core.StopActive(); err != nil {
				app.Logger.Error("tray: menu — restart stop failed", "err", err)
			}
			if _, err := core.StartActive(); err != nil {
				app.Logger.Error("tray: menu — restart start failed", "err", err)
			}
		},
		quit: func() {
			// Quitting must not strand the role: a running server or
			// client would keep sharing input with the other machine (and,
			// on Windows, keep the elevated client's input gate on) with
			// no tray left to control it.
			app.Logger.Info("tray: menu — quit")
			_ = core.StopAll()
			app.Quit()
		},
	}

	// Seed the menu labels, then keep them current for the tray's life.
	// The C thread reads them via kvmTrayState each time the menu opens.
	refreshXEmbedState(core)
	go func() {
		ticker := time.NewTicker(time.Second)
		defer ticker.Stop()
		for range ticker.C {
			refreshXEmbedState(core)
		}
	}()

	// The C thread parks in its event loop for the process lifetime.
	go func() {
		data := C.CBytes(icon.rgba)
		defer C.free(data)
		app.Logger.Info("tray: XEmbed icon thread starting", "size", icon.w)
		ok := C.xembed_tray_run((*C.uchar)(data), C.int(icon.w), C.int(icon.h))
		if ok == 0 {
			app.Logger.Info("tray: XEmbed fallback unavailable — no X display")
		} else {
			app.Logger.Info("tray: XEmbed icon thread exited")
		}
	}()
}

// refreshXEmbedState pushes the current role state into the shared struct
// that the C menu reads (via kvmTrayState).
func refreshXEmbedState(core *App) {
	st := computeTrayState(core)
	role := "server"
	if st.role == ModeClient {
		role = "client"
	}
	setXEmbedState(role, st.running)
}

// xembedActions are the popup menu's actions, wired once in
// setupXEmbedTray before the icon thread starts and read from the C
// thread through the //export callbacks below. Assigned-before-start, so
// no locking is needed.
type xembedActions struct {
	open    func()
	start   func()
	stop    func()
	restart func()
	quit    func()
}

var xembedAct xembedActions

// xembedState is the role status the C menu renders. It is written by
// the refresh ticker (Go) and read by the C thread via kvmTrayState when
// the menu opens — the mutex keeps the two safe.
type xembedState struct {
	mu      sync.Mutex
	role    string // "server" | "client"
	running bool
}

var xembedSt xembedState

func setXEmbedState(role string, running bool) {
	xembedSt.mu.Lock()
	xembedSt.role = role
	xembedSt.running = running
	xembedSt.mu.Unlock()
}

func getXEmbedState() (string, bool) {
	xembedSt.mu.Lock()
	defer xembedSt.mu.Unlock()
	return xembedSt.role, xembedSt.running
}

//export kvmTrayState
func kvmTrayState() *C.char {
	role, running := getXEmbedState()
	flag := "0"
	if running {
		flag = "1"
	}
	return C.CString(role + "|" + flag)
}

//export kvmTrayOpen
func kvmTrayOpen() { if xembedAct.open != nil { xembedAct.open() } }

//export kvmTrayStart
func kvmTrayStart() { if xembedAct.start != nil { xembedAct.start() } }

//export kvmTrayStop
func kvmTrayStop() { if xembedAct.stop != nil { xembedAct.stop() } }

//export kvmTrayRestart
func kvmTrayRestart() { if xembedAct.restart != nil { xembedAct.restart() } }

//export kvmTrayQuit
func kvmTrayQuit() { if xembedAct.quit != nil { xembedAct.quit() } }

// trayPixels is a decoded icon as raw RGBA.
type trayPixels struct {
	rgba []byte
	w, h int
}

// decodeTrayIcon decodes the embedded tray.png to RGBA.
func decodeTrayIcon() (trayPixels, error) {
	img, err := png.Decode(bytes.NewReader(trayIcon))
	if err != nil {
		return trayPixels{}, err
	}
	b := img.Bounds()
	w, h := b.Dx(), b.Dy()
	rgba := make([]byte, w*h*4)
	// image/png decodes to *image.NRGBA or *image.RGBA; both store 8-bit
	// channels in this layout, but NRGBA is non-premultiplied which is
	// what the C side expects.
	switch src := img.(type) {
	case *image.NRGBA:
		copy(rgba, src.Pix)
	default:
		// Convert any other model through NRGBA.
		for y := 0; y < h; y++ {
			for x := 0; x < w; x++ {
				r, g, bb, a := img.At(b.Min.X+x, b.Min.Y+y).RGBA()
				i := (y*w + x) * 4
				rgba[i] = byte(r >> 8)
				rgba[i+1] = byte(g >> 8)
				rgba[i+2] = byte(bb >> 8)
				rgba[i+3] = byte(a >> 8)
			}
		}
	}
	return trayPixels{rgba: rgba, w: w, h: h}, nil
}