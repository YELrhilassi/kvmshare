//go:build linux

package main

// XEmbed tray popup menu, implemented directly on Xlib.
//
// XEmbed has no menu protocol, so the icon's right-click opens this small
// override-redirect window with a pointer+keyboard grab. It is transient by
// design: built per click, destroyed on selection or dismiss. The labels are
// rebuilt from Go state (via kvmTrayState) every time the menu opens, and
// the chosen row is dispatched by the icon thread's event loop.
//
// This file is the menu half of the XEmbed fallback: it shares the icon
// No state is shared with the icon thread: the display is passed in on
// every call, and the start/stop decision is returned through an out
// parameter (cgo copies the preamble of any file with //export functions
// into a header every cgo file includes, so a global here would collide
// with the icon file's copy at link time).

/*
#cgo pkg-config: x11
#include <X11/Xlib.h>
#include <X11/cursorfont.h>
#include <X11/keysym.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

extern char *kvmTrayState(void); // Go //export; also declared in the cgo header

#define MENU_W 190
#define MENU_ITEM_H 26
#define MENU_PAD 4
#define MENU_ITEMS 5

static Window menu_win = 0;
static int menu_running = 0;    // role running? (from kvmTrayState)
static char menu_state[64];     // "server · running" row
static char menu_startstop[64]; // "Start server" / "Stop server"

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
static void menu_draw(Display *dpy, XFontStruct *fs, int hover) {
	if (menu_win == 0) return;
	GC gc = DefaultGC(dpy, DefaultScreen(dpy));
	int h = menu_height();
	XSetForeground(dpy, gc, 0x1b1b1f);
	XFillRectangle(dpy, menu_win, gc, 0, 0, MENU_W, h);
	// Separator under the state row.
	XSetForeground(dpy, gc, 0x3a3a40);
	XFillRectangle(dpy, menu_win, gc, 10, MENU_PAD + MENU_ITEM_H - 1, MENU_W - 20, 1);
	for (int i = 0; i < MENU_ITEMS; i++) {
		const char *text = menu_item_text(i);
		if (text == NULL) continue;
		int y0 = MENU_PAD + i * MENU_ITEM_H;
		if (i == hover) {
			XSetForeground(dpy, gc, 0x2d6cf6);
			XFillRectangle(dpy, menu_win, gc, 2, y0, MENU_W - 4, MENU_ITEM_H);
			XSetForeground(dpy, gc, 0xffffff);
		} else if (i == 0) {
			XSetForeground(dpy, gc, 0x8a8a90);
		} else {
			XSetForeground(dpy, gc, 0xe8e8ea);
		}
		int ty = y0 + (MENU_ITEM_H + fs->ascent - fs->descent) / 2;
		XDrawString(dpy, menu_win, gc, 12, ty, text, (int)strlen(text));
	}
	XFlush(dpy);
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
// *was_running reports the role state captured when the menu opened, so
// the caller can pick Start vs Stop. Non-static: the icon file calls it
// via extern, and tray_menu_linux.go's Go half references it below to
// keep this translation unit in the link (cgo drops cgo files whose Go
// code never touches a C symbol).
int menu_run(Display *dpy, int px, int py, int *was_running) {
	int scr = DefaultScreen(dpy);
	int sw = WidthOfScreen(DefaultScreenOfDisplay(dpy));
	int sh = HeightOfScreen(DefaultScreenOfDisplay(dpy));
	int mw = MENU_W, mh = menu_height();
	if (px + mw > sw) px = sw - mw;
	if (py + mh > sh) py = sh - mh;
	if (px < 0) px = 0;
	if (py < 0) py = 0;

	menu_refresh_labels();

	menu_win = XCreateSimpleWindow(dpy, RootWindow(dpy, scr),
		px, py, mw, mh, 1, 0x4a4a50, 0x1b1b1f);
	XStoreName(dpy, menu_win, "kvmshare");
	XSetWindowAttributes attrs;
	attrs.override_redirect = True;
	XChangeWindowAttributes(dpy, menu_win, CWOverrideRedirect, &attrs);
	XSelectInput(dpy, menu_win, ExposureMask | ButtonPressMask |
		ButtonReleaseMask | PointerMotionMask | KeyPressMask);
	XMapRaised(dpy, menu_win);

	XFontStruct *fs = XLoadQueryFont(dpy, "fixed");
	if (fs == NULL) {
		XDestroyWindow(dpy, menu_win);
		menu_win = 0;
		return 0;
	}

	Cursor arrow = XCreateFontCursor(dpy, XC_left_ptr);
	if (XGrabPointer(dpy, menu_win, False,
		ButtonPressMask | ButtonReleaseMask | PointerMotionMask,
		GrabModeAsync, GrabModeAsync, None, arrow, CurrentTime) != GrabSuccess) {
		XFreeFont(dpy, fs);
		XFreeCursor(dpy, arrow);
		XDestroyWindow(dpy, menu_win);
		menu_win = 0;
		return 0;
	}
	// A keyboard grab is best-effort: Escape-to-dismiss is nice but not
	// worth failing the menu over if another app holds the keyboard.
	XGrabKeyboard(dpy, menu_win, False, GrabModeAsync, GrabModeAsync, CurrentTime);
	XFlush(dpy);

	int hover = -1, armed = 0, action = 0, done = 0;
	menu_draw(dpy, fs, hover);

	while (!done) {
		XEvent ev;
		XNextEvent(dpy, &ev);
		switch (ev.type) {
		case Expose:
			menu_draw(dpy, fs, hover);
			break;
		case MotionNotify: {
			int i = menu_item_at(ev.xmotion.x, ev.xmotion.y);
			if (i != hover) { hover = i; menu_draw(dpy, fs, hover); }
			break;
		}
		case ButtonPress: {
			int i = menu_item_at(ev.xbutton.x, ev.xbutton.y);
			if (i > 0) { armed = i; menu_draw(dpy, fs, hover); }
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

	XUngrabKeyboard(dpy, CurrentTime);
	XUngrabPointer(dpy, CurrentTime);
	XDestroyWindow(dpy, menu_win);
	menu_win = 0;
	XFreeCursor(dpy, arrow);
	XFreeFont(dpy, fs);
	XFlush(dpy);
	return action;
}
*/
import "C"

// Keep this cgo file's preamble in the final link: the icon thread's C
// event loop calls menu_run, and a cgo file whose Go half never references
// a C symbol is dropped by the toolchain (undefined reference at link).
var _ = C.menu_run
