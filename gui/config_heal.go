package main

// config_heal.go — repair config files written by older GUI releases so
// they parse under the strict schema the Rust server enforces.
//
// The old writer round-tripped the [shortcuts] and [input] sections as
// generic key/value maps through JavaScript, where every number is an
// f64. The file therefore gained `key = 71.0` (the Rust schema wants an
// integer HID usage) and camelCase input names (`pointerSpeed` — the
// schema is snake_case). Strict decoding then rejected the whole
// sections: bindings never registered and input feel silently reset.
//
// Healing converts integral floats to integers and camelCase input
// names to the file's snake_case — once, in place — so the next load
// parses strictly. A file that is broken in some *other* way is left
// untouched; healing is not a license to guess.

import (
	"bytes"
	"regexp"
)

var (
	// `identifier = <number>` at line scope. Go-toml writes maps with
	// `key = value` on one line, which is all the old writer produced.
	configFloatLine = regexp.MustCompile(`(?m)^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([0-9]+\.[0-9]+)\s*$`)
	// The old `[input]` names are the camelCase shapes the frontend
	// uses. Renamed to the file's snake_case schema.
	inputRenames = []struct{ from, to string }{
		{"pointerSpeed", "pointer_speed"},
		{"wheelSpeed", "wheel_speed"},
		{"swapScroll", "swap_scroll"},
	}
)

// healConfigText rewrites a config produced by an old GUI release into
// the strict on-disk schema. It reports whether it changed anything.
func healConfigText(raw []byte) ([]byte, error) {
	changed := false

	// Integral floats → integers. A float with a real fraction (or an
	// absurd magnitude for a HID id) stays untouched: that is genuinely
	// corrupt data, not a writer quirk, and the parse error should
	// surface rather than be papered over.
	out := configFloatLine.ReplaceAllFunc(raw, func(m []byte) []byte {
		sub := configFloatLine.FindSubmatch(m)
		parts := bytes.SplitN(sub[2], []byte("."), 2)
		intPart, frac := parts[0], parts[1]
		if len(intPart) == 0 || len(intPart) > 9 || !allZeros(frac) {
			return m
		}
		changed = true
		return append(append(sub[1], []byte(" = ")...), intPart...)
	})

	// camelCase [input] names → snake_case, inside the section only.
	if start, ok := sectionRange(out, "input"); ok {
		section := out[start.lo:start.hi]
		for _, r := range inputRenames {
			fixed, n := rewriteSectionKey(section, r.from, r.to)
			if n > 0 {
				changed = true
				section = fixed
			}
		}
		out = append(out[:start.lo], append(section, out[start.hi:]...)...)
	}

	if !changed {
		return raw, nil
	}
	return out, nil
}

// sectionRange finds `[name]` and returns the byte range of its body
// (up to the next section header or EOF).
func sectionRange(b []byte, name string) (struct{ lo, hi int }, bool) {
	var r struct{ lo, hi int }
	hdr := []byte("[" + name + "]")
	idx := bytes.Index(b, hdr)
	if idx < 0 {
		return r, false
	}
	r.lo = idx + len(hdr)
	end := bytes.Index(b[r.lo:], []byte("\n["))
	if end < 0 {
		r.hi = len(b)
	} else {
		r.hi = r.lo + end + 1 // keep the newline that begins the next header
	}
	return r, true
}

// rewriteSectionKey renames `from =` to `to =` on every top-level line
// of a section body.
func rewriteSectionKey(body []byte, from, to string) ([]byte, int) {
	needle := []byte(from + " =")
	repl := []byte(to + " =")
	n := bytes.Count(body, needle)
	if n == 0 {
		return body, 0
	}
	return bytes.ReplaceAll(body, needle, repl), n
}

// allZeros reports whether every byte is '0' (an integral float's
// fraction, e.g. the `0` in `71.0`).
func allZeros(b []byte) bool {
	for _, c := range b {
		if c != '0' {
			return false
		}
	}
	return true
}
