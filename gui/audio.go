package main

// audio.go — the GUI's view of audio: the device pickers, the live link
// state, and the "is this actually working?" test.
//
// Three things the GUI needs and cannot compute itself:
//
//   - **the device lists** — the enumeration lives in the Rust platform
//     layer (`kvmshare-platform`), which a Go program cannot link. So the
//     GUI asks the server binary (`--audio-devices`), the one place that
//     already knows how to speak to PulseAudio/PipeWire and WASAPI. One
//     implementation, both platforms, no second pactl parser in Go.
//
//   - **the live link state** — the runtime that owns the stream lives in
//     the role process, not here. It writes `<state_dir>/audio.state` on
//     every transition (see crates/app/src/audio_state.rs), exactly like
//     `client.state`, and this file folds that into the live snapshot.
//     The file also carries the two meters, which is what lets the page say
//     *what is being captured* rather than only that a switch is on.
//
//   - **the test tone** — playing a sound needs the same platform layer, so
//     the role binary is asked for that too (`--audio-test-tone`). The GUI
//     plays the tone and watches the meters above while it plays; see
//     TestAudio.

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// AudioDevices is the picker data: the output-monitor sources capture can
// use, and the output devices playback can use. Names are opaque platform
// strings; an empty selection always means "the system default".
type AudioDevices struct {
	Capture  []string `json:"capture"`
	Playback []string `json:"playback"`
}

// ListAudioDevices asks the role binary for this machine's audio devices.
//
// A machine without audio hardware, or one where the audio server tools
// are missing, answers with empty lists rather than an error — the picker
// then simply offers "system default", which is the honest state.
func (a *mediaService) ListAudioDevices() (AudioDevices, error) {
	bin := a.serverPath
	if bin == "" {
		return AudioDevices{}, errors.New("kvmshare-server was not found")
	}
	// The query is local and fast; the timeout only exists so a wedged
	// binary can never hang the settings page.
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, bin, "--audio-devices").Output()
	if err != nil {
		return AudioDevices{}, fmt.Errorf("list audio devices: %w", err)
	}
	return parseAudioDevices(out)
}

// parseAudioDevices decodes the `--audio-devices` JSON. Split out so the
// contract with the Rust side is testable without a binary.
func parseAudioDevices(out []byte) (AudioDevices, error) {
	var devs AudioDevices
	if err := json.Unmarshal(out, &devs); err != nil {
		return AudioDevices{}, fmt.Errorf("list audio devices: unexpected output: %w", err)
	}
	// Never hand a nil slice to the frontend: it renders as `null` and
	// forces every consumer to guard. An empty list renders as an empty
	// picker, which is the truth.
	if devs.Capture == nil {
		devs.Capture = []string{}
	}
	if devs.Playback == nil {
		devs.Playback = []string{}
	}
	return devs, nil
}

// AudioState is the live audio link as the role process reports it.
type AudioState struct {
	// Configured: this machine is set to send / receive.
	Send    bool `json:"send"`
	Receive bool `json:"receive"`
	// Streaming: a direction is actually flowing right now.
	Sending   bool `json:"sending"`
	Receiving bool `json:"receiving"`
	// Peer is the machine at the other end, as its owner names it ("hp").
	// Empty until a link is settled, or when the owner knows no name.
	Peer string `json:"peer"`
	// CapturePlaying: the sound being captured is above the silence floor.
	// False with Sending true is a normal state — a live link with a quiet
	// machine — which is exactly why it is reported separately.
	CapturePlaying bool `json:"capturePlaying"`
	// CaptureLevelDb: the most recent captured level. Null = no reading
	// (nothing is being captured), which is not the same as silence.
	CaptureLevelDb *float64 `json:"captureLevelDb"`
	// ReceivePlaying / ReceiveLevelDb: the same for what is being played —
	// a level above the floor is proof the peer's audio is arriving, not
	// merely announced.
	ReceivePlaying bool     `json:"receivePlaying"`
	ReceiveLevelDb *float64 `json:"receiveLevelDb"`
	// CaptureNote is a non-fatal warning about the capture path — "this
	// output is muted", "that monitor's recording gain is at 21%" — empty
	// when there is nothing to say. It is the difference between a user
	// seeing "silent" and a user knowing which setting to change.
	//
	// Not an error: the link is working exactly as configured. Only some
	// platforms can answer it (Linux today), so an empty note is not a
	// promise that all is well.
	CaptureNote string `json:"captureNote"`
	// Error is the last failure ("capture stopped: …"), empty when none.
	// This is what turns a silent stream into a visible problem.
	Error string `json:"error"`
	// Active is derived: a role is running and audio is configured at all.
	// The page uses it to decide whether to show the status line.
	Active bool `json:"active"`
}

// audioStateFile is the state file name, relative to the state dir. It must
// match crates/app/src/audio_state.rs.
const audioStateFile = "audio.state"

// audioFileState is one parse of the state file: every key the Rust side
// writes. One parser, so the live snapshot, the meter the test button
// watches, and any future reader can never disagree about what the file
// says. A level is a pointer because "no reading" and "silence" are
// different facts and the page renders them differently.
type audioFileState struct {
	send, receive                  bool
	sending, receiving             bool
	capturePlaying, receivePlaying bool
	captureLevel, receiveLevel     *float64
	peer, errText, captureNote     string
}

// parseAudioStateFile decodes the trivial key=value body. Unknown keys are
// ignored (a newer role may write more than this GUI knows) and an
// unparsable line is skipped: a state file is observability, and a torn or
// partly-written one must degrade to "less is known", never to an error the
// page has to show.
func parseAudioStateFile(raw []byte) audioFileState {
	var st audioFileState
	for _, line := range strings.Split(string(raw), "\n") {
		kv := strings.SplitN(strings.TrimSpace(line), "=", 2)
		if len(kv) != 2 {
			continue
		}
		value := kv[1]
		switch kv[0] {
		case "send":
			st.send = value == "1"
		case "receive":
			st.receive = value == "1"
		case "sending":
			st.sending = value == "1"
		case "receiving":
			st.receiving = value == "1"
		case "capture_playing":
			st.capturePlaying = value == "1"
		case "receive_playing":
			st.receivePlaying = value == "1"
		case "capture_level":
			st.captureLevel = parseLevel(value)
		case "receive_level":
			st.receiveLevel = parseLevel(value)
		case "peer":
			st.peer = value
		case "capture_note":
			st.captureNote = value
		case "error":
			st.errText = value
		}
	}
	return st
}

// parseLevel reads a dBFS value, or nil for anything that is not a level.
//
// The empty value is "no reading". So is a non-numeric one (a future
// format, a torn write) and so is a non-finite one: `strconv` happily
// parses `-inf` and `nan` — the exact strings a level *would* be if the
// clamping ever moved to this side — and a NaN or infinite number drawn on
// a meter is worse than an honest blank.
func parseLevel(value string) *float64 {
	if strings.TrimSpace(value) == "" {
		return nil
	}
	db, err := strconv.ParseFloat(value, 64)
	if err != nil || math.IsNaN(db) || math.IsInf(db, 0) {
		return nil
	}
	return &db
}

// readAudioStateFile reads and parses the state file. A missing file is
// `false` — "nothing configured", which every caller renders as the inert
// default rather than as a link.
func readAudioStateFile(dir string) (audioFileState, bool) {
	raw, err := os.ReadFile(filepath.Join(dir, audioStateFile))
	if err != nil {
		return audioFileState{}, false
	}
	return parseAudioStateFile(raw), true
}

// AudioStatus reads the audio link state for a role that is (or is not)
// running. A missing file means "nothing configured"; a file left by a
// process that is no longer running is reconciled away rather than shown
// as a live stream.
func (a *App) AudioStatus(roleRunning bool) AudioState {
	var st AudioState
	parsed, ok := readAudioStateFile(a.stateDir)
	if !ok {
		return st
	}
	st.Send = parsed.send
	st.Receive = parsed.receive
	st.Sending = parsed.sending
	st.Receiving = parsed.receiving
	st.Peer = parsed.peer
	st.CapturePlaying = parsed.capturePlaying
	st.CaptureLevelDb = parsed.captureLevel
	st.ReceivePlaying = parsed.receivePlaying
	st.ReceiveLevelDb = parsed.receiveLevel
	st.CaptureNote = parsed.captureNote
	st.Error = parsed.errText
	// No process, no link: a crashed role leaves its last state behind,
	// and claiming a stream that is not there is exactly the kind of
	// stale lie the connection panel already guards against. A meter is
	// part of the stream, so it goes with it.
	if !roleRunning {
		st.Sending = false
		st.Receiving = false
		st.CapturePlaying = false
		st.ReceivePlaying = false
		st.CaptureLevelDb = nil
		st.ReceiveLevelDb = nil
	}
	st.Active = roleRunning && (st.Send || st.Receive)
	return st
}

// AudioTestResult is the answer to "does audio actually work on this
// machine?". It is deliberately two facts rather than one, because the
// failures are different and want different fixes:
//
//   - Played — a tone was produced on this machine's output. If it was not,
//     the output device is the problem (Error says why).
//   - CaptureHeard — the running role's capture meter saw that tone. That is
//     the loopback the link actually streams; a tone that plays but is never
//     captured means the output works and the capture path does not.
type AudioTestResult struct {
	Played bool   `json:"played"`
	Device string `json:"device"`
	// Error is why no tone was played (empty when it was).
	Error string `json:"error"`
	// Sending/Receiving: a direction was streaming while the test ran. With
	// neither, the tone was a plain speaker check — honest, but it says
	// nothing about the link.
	Sending   bool `json:"sending"`
	Receiving bool `json:"receiving"`
	// CaptureHeard: the capture meter saw sound while the tone played.
	CaptureHeard bool `json:"captureHeard"`
	// PeakDb is the highest captured level seen during the test; HasPeak is
	// false when nothing was being captured at all (so the number means
	// nothing and the page must not print it).
	PeakDb  float64 `json:"peakDb"`
	HasPeak bool    `json:"hasPeak"`
}

// How long the test tone plays, and how often the meters are sampled while
// it does. The tone is long enough to be heard and to show up on a meter,
// short enough that pressing the button feels immediate.
const (
	audioTestSeconds = 1.5
	audioTestSample  = 80 * time.Millisecond
)

// toneRequest is the `--audio-test-tone` JSON, produced by
// crates/app/src/audio_test.rs. A cross-language contract, so it is a
// struct here rather than a map: a rename on either side must break a test
// rather than the button.
type toneRequest struct {
	OK         bool    `json:"ok"`
	Device     string  `json:"device"`
	Seconds    float64 `json:"seconds"`
	SampleRate int     `json:"sampleRate"`
	Channels   int     `json:"channels"`
	Error      string  `json:"error"`
}

// TestAudio plays a short tone on this machine's output and reports whether
// the audio path saw it.
//
// The binary asked is the one belonging to this machine's *role* — a client
// asking its server binary would play a tone on a machine it is not running
// on. `device` is the configured playback device (empty = system default),
// so the test exercises the device a link would actually use.
func (a *mediaService) TestAudio(device string) (AudioTestResult, error) {
	a.mu.Lock()
	mode := a.settings.Mode
	a.mu.Unlock()
	bin := a.serverPath
	if mode == ModeClient {
		bin = a.clientPath
	}
	if bin == "" {
		return AudioTestResult{}, errors.New("the role binary was not found (run make install)")
	}

	args := []string{"--audio-test-tone", "--seconds", strconv.FormatFloat(audioTestSeconds, 'f', 1, 64)}
	if device != "" {
		args = append(args, "--device", device)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, bin, args...)
	var stdout bytes.Buffer
	cmd.Stdout = &stdout
	if err := cmd.Start(); err != nil {
		return AudioTestResult{Device: device, Error: err.Error()}, nil
	}

	// Watch the running role's meters while the tone plays. This runs
	// alongside the tone rather than after it because the capture reading
	// only exists while sound is actually flowing.
	done := make(chan error, 1)
	go func() { done <- cmd.Wait() }()
	sampled := make(chan AudioTestResult, 1)
	go func() {
		sampled <- sampleWhilePlaying(a.stateDir, done)
	}()

	observed := <-sampled
	// The sampler returns when the tone process does, so the exit status is
	// available without a second wait.
	printed, decodeErr := parseToneRequest(stdout.Bytes())
	observed.Device = device
	switch {
	case decodeErr != nil:
		observed.Played = false
		observed.Error = fmt.Sprintf("the tone command produced unexpected output: %v", decodeErr)
	case !printed.OK:
		observed.Error = printed.Error
		if observed.Error == "" {
			observed.Error = "the output device refused the tone"
		}
	default:
		observed.Played = true
		if printed.Device != "" {
			observed.Device = printed.Device
		}
	}
	return observed, nil
}

// sampleWhilePlaying polls the state file until the tone process exits,
// collecting the loudest capture reading and whether either direction was
// streaming. Split from TestAudio so the "watch a level" loop is one small
// piece.
func sampleWhilePlaying(stateDir string, done <-chan error) AudioTestResult {
	var result AudioTestResult
	peak := math.Inf(-1)
	ticker := time.NewTicker(audioTestSample)
	defer ticker.Stop()
	for {
		select {
		case <-done:
			if !math.IsInf(peak, -1) {
				result.PeakDb = peak
				result.HasPeak = true
			}
			return result
		case <-ticker.C:
			parsed, ok := readAudioStateFile(stateDir)
			if !ok {
				continue
			}
			if parsed.sending {
				result.Sending = true
			}
			if parsed.receiving {
				result.Receiving = true
			}
			if parsed.captureLevel != nil && *parsed.captureLevel > peak {
				peak = *parsed.captureLevel
			}
			if parsed.capturePlaying {
				result.CaptureHeard = true
			}
		}
	}
}

// parseToneRequest decodes the `--audio-test-tone` JSON. Split out so the
// contract with the Rust side is testable without a sound card.
func parseToneRequest(out []byte) (toneRequest, error) {
	var tone toneRequest
	if err := json.Unmarshal(out, &tone); err != nil {
		return toneRequest{}, err
	}
	return tone, nil
}
