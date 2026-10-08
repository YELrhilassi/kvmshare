package main

// audio_test.go — the GUI's audio surface: the device-list contract with
// the Rust binary, and the live-state reconciliation. Both are places
// where a wrong parse would show a user a device that cannot work or a
// stream that is not running.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// The JSON the server binary prints is the contract; a change on either
// side must break here rather than in the picker.
func TestParseAudioDevices(t *testing.T) {
	devs, err := parseAudioDevices([]byte(`{"capture":["a.monitor"],"playback":["a","b"]}`))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if len(devs.Capture) != 1 || devs.Capture[0] != "a.monitor" {
		t.Errorf("capture = %v", devs.Capture)
	}
	if len(devs.Playback) != 2 {
		t.Errorf("playback = %v", devs.Playback)
	}
}

// Empty lists are normal (no audio server / no devices) and must stay
// non-nil so the frontend never sees `null`.
func TestParseAudioDevicesEmptyListsAreNotNil(t *testing.T) {
	devs, err := parseAudioDevices([]byte(`{}`))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if devs.Capture == nil || devs.Playback == nil {
		t.Error("empty device lists must be [] not nil")
	}
}

func TestParseAudioDevicesRejectsGarbage(t *testing.T) {
	if _, err := parseAudioDevices([]byte("not json")); err == nil {
		t.Fatal("garbage output must be an error, not empty lists")
	}
}

func TestListAudioDevicesWithoutBinaryIsAnError(t *testing.T) {
	a := testApp(&App{})
	if _, err := a.ListAudioDevices(); err == nil {
		t.Fatal("no server binary must be a reported error")
	}
}

// The state file is read as written by crates/app/src/audio_state.rs.
func TestAudioStatusReadsTheStateFile(t *testing.T) {
	dir := t.TempDir()
	body := "send=1\nreceive=0\nsending=1\nreceiving=0\nerror=capture stopped: device gone\n"
	if err := os.WriteFile(filepath.Join(dir, "audio.state"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	a := testApp(&App{stateDir: dir})
	st := a.AudioStatus(true)
	if !st.Send || st.Receive {
		t.Errorf("send/receive = %v/%v, want true/false", st.Send, st.Receive)
	}
	if !st.Sending || st.Receiving {
		t.Errorf("sending/receiving = %v/%v, want true/false", st.Sending, st.Receiving)
	}
	if !strings.Contains(st.Error, "capture stopped") {
		t.Errorf("error = %q", st.Error)
	}
	if !st.Active {
		t.Error("a running role with audio configured must be Active")
	}
}

// A missing file is the inert default: nothing configured, not active.
func TestAudioStatusMissingFileIsInactive(t *testing.T) {
	a := testApp(&App{stateDir: t.TempDir()})
	st := a.AudioStatus(true)
	if st.Active || st.Send || st.Receive || st.Sending || st.Receiving {
		t.Errorf("missing state file must be the inert default, got %+v", st)
	}
}

// The meter fields are what the page shows as "what is being captured";
// they must survive the round trip, and a missing reading must be absent
// rather than a zero (which would draw a full-scale meter out of nothing).
func TestAudioStatusReadsTheMetersAndPeer(t *testing.T) {
	dir := t.TempDir()
	body := "send=1\nreceive=0\nsending=1\nreceiving=0\nerror=\npeer=hp\n" +
		"capture_playing=1\ncapture_level=-23.4\nreceive_playing=0\nreceive_level=\n" +
		"capture_note=the output is muted\n"
	if err := os.WriteFile(filepath.Join(dir, "audio.state"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	a := testApp(&App{stateDir: dir})
	st := a.AudioStatus(true)
	if st.Peer != "hp" {
		t.Errorf("peer = %q, want hp", st.Peer)
	}
	if !st.CapturePlaying {
		t.Error("capture_playing must be reported")
	}
	if st.CaptureLevelDb == nil || *st.CaptureLevelDb != -23.4 {
		t.Errorf("capture level = %v, want -23.4", st.CaptureLevelDb)
	}
	if st.ReceivePlaying {
		t.Error("receive_playing must be false")
	}
	if st.ReceiveLevelDb != nil {
		t.Errorf("an empty level is no reading, got %v", st.ReceiveLevelDb)
	}
	// The warning about the capture path is what tells a user which setting
	// to change, so it must survive the round trip with its words intact.
	if st.CaptureNote != "the output is muted" {
		t.Errorf("capture note = %q", st.CaptureNote)
	}
}

// A meter belongs to a stream: with no role running there is nothing being
// captured or played, and a stale level on the page would be a lie.
func TestAudioStatusClearsMetersWithoutARole(t *testing.T) {
	dir := t.TempDir()
	body := "send=1\nsending=1\ncapture_playing=1\ncapture_level=-10.0\n"
	if err := os.WriteFile(filepath.Join(dir, "audio.state"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	a := testApp(&App{stateDir: dir})
	st := a.AudioStatus(false)
	if st.CaptureLevelDb != nil || st.CapturePlaying || st.Sending {
		t.Errorf("no running role must clear the meters, got %+v", st)
	}
}

// A level that is not a finite number is "no reading" rather than a value:
// neither a torn write nor a literal `-inf` may reach a meter.
func TestAudioStatusIgnoresANonFiniteLevel(t *testing.T) {
	parsed := parseAudioStateFile([]byte("capture_level=-inf\nreceive_level=nan\n"))
	if parsed.captureLevel != nil || parsed.receiveLevel != nil {
		t.Errorf("non-finite levels must be absent, got %v/%v", parsed.captureLevel, parsed.receiveLevel)
	}
	if level := parseLevel("-23.5"); level == nil || *level != -23.5 {
		t.Errorf("a finite level must parse, got %v", level)
	}
}

// The tone command's JSON is a cross-language contract; a change on either
// side must break here rather than in the button.
func TestParseToneRequest(t *testing.T) {
	tone, err := parseToneRequest([]byte(`{"ok":true,"device":"hw:0","seconds":1.5,"sampleRate":48000,"channels":2}`))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if !tone.OK || tone.Device != "hw:0" || tone.SampleRate != 48000 || tone.Channels != 2 {
		t.Errorf("tone = %+v", tone)
	}
	failure, err := parseToneRequest([]byte(`{"ok":false,"device":"","error":"no such device"}`))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if failure.OK || failure.Error != "no such device" {
		t.Errorf("failure = %+v", failure)
	}
	if _, err := parseToneRequest([]byte("not json")); err == nil {
		t.Fatal("garbage output must be an error")
	}
}

// Without a resolved binary the test must say so rather than pretending to
// have played a tone.
func TestTestAudioWithoutABinaryIsAnError(t *testing.T) {
	a := testApp(&App{stateDir: t.TempDir()})
	if _, err := a.TestAudio(""); err == nil {
		t.Fatal("no role binary must be a reported error")
	}
}

// A file left behind by a crashed role must never read as a live stream.
func TestAudioStatusReconcilesADeadRole(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "audio.state"), []byte("send=1\nreceive=1\nsending=1\nreceiving=1\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	a := testApp(&App{stateDir: dir})
	st := a.AudioStatus(false)
	if st.Sending || st.Receiving {
		t.Errorf("no running role must clear the stream, got %+v", st)
	}
	if st.Active {
		t.Error("no running role must not be Active")
	}
	// The configuration intent survives: it is the stream that is gone.
	if !st.Send || !st.Receive {
		t.Errorf("the configured intent should remain readable, got %+v", st)
	}
}
