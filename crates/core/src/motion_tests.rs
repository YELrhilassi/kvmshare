use super::*;

#[test]
fn keeps_fractional_remainder() {
    let mut m = PendingMotion::default();
    // Sub-pixel motion is held until it crosses a full pixel, with
    // no overshoot in either direction: -0.9px stays put...
    m.push(-0.3, 0.0);
    m.push(-0.3, 0.0);
    m.push(-0.3, 0.0);
    assert_eq!(m.take_whole(), None);
    assert!((m.fx + 0.9).abs() < 1e-9);
    // ...until it crosses -1, and then it emits exactly -1 (a floor
    // here would have emitted -1 already at -0.9 and biased the
    // cursor leftward under jitter).
    m.push(-0.3, 0.0);
    assert_eq!(m.take_whole(), Some((-1, 0)));
    assert!((m.fx + 0.2).abs() < 1e-9);
    // Nothing more to take until more motion accrues.
    assert_eq!(m.take_whole(), None);
    // Fast motion passes through whole (the leftover -0.2 from the
    // slow drift above offsets the 12: 11.8 -> 11).
    m.push(12.0, 5.9);
    assert_eq!(m.take_whole(), Some((11, 5)));
    assert!((m.fx - 0.8).abs() < 1e-9);
    assert!((m.fy - 0.9).abs() < 1e-9);
}

#[test]
fn has_pending_sees_only_whole_pixels() {
    let mut m = PendingMotion::default();
    assert!(!m.has_pending());
    m.push(0.4, 0.0);
    assert!(!m.has_pending(), "sub-pixel motion is not pending");
    m.push(0.7, 0.0);
    assert!(m.has_pending(), "a whole pixel is pending");
    assert_eq!(m.take_whole(), Some((1, 0)));
    assert!(!m.has_pending());
}

/// Simulate a closed-loop plant: `gain` is the OS transform (1.0 = a
/// faithful 1:1 OS, 2.0 = Windows EPP doubling fast input). Each
/// `correct` injection moves the simulated real cursor by
/// `gain`×counts, then the caller re-reads it — exactly how the real
/// loop behaves.
struct Plant {
    real: (f64, f64),
    gain: f64,
}

impl Plant {
    fn new(gain: f64) -> Self {
        Self { real: (0.0, 0.0), gain }
    }
    fn move_rel(&mut self, dx: i32, dy: i32) {
        self.real.0 += dx as f64 * self.gain;
        self.real.1 += dy as f64 * self.gain;
    }
    fn position(&self) -> (i32, i32) {
        (self.real.0.round() as i32, self.real.1.round() as i32)
    }
}

/// Drive a follower to convergence against a plant, the way the
/// client loop does: push a feedforward frame, then tick `correct`
/// until the real cursor stops moving.
fn converge(f: &mut PositionFollower, plant: &mut Plant, max_ticks: usize) -> usize {
    for i in 0..max_ticks {
        let before = plant.position();
        if let Some((dx, dy)) = f.correct(before) {
            plant.move_rel(dx, dy);
        }
        let after = plant.position();
        if before == after && f.error(before) == (0, 0) {
            return i;
        }
    }
    max_ticks
}

#[test]
fn follower_tracks_command_and_absorbs_os_gain() {
    // Half feedforward on a 1:1 OS: the frame lands halfway, and the
    // closed loop delivers the rest. Converges, no overshoot.
    let mut f = PositionFollower::default();
    f.enter(100, 100);
    let mut plant = Plant::new(1.0);
    plant.real = (100.0, 100.0);
    let (dx, dy) = f.push(50, -20);
    plant.move_rel(dx, dy);
    assert_eq!(plant.position(), (125, 90));
    assert_eq!(f.error(plant.position()), (25, -10));
    let ticks = converge(&mut f, &mut plant, 200);
    assert!(ticks < 200, "follower must converge");
    assert_eq!(f.error(plant.position()), (0, 0));
    // Settled: no more corrections (no limit cycle).
    assert_eq!(f.correct(plant.position()), None);

    // A 2x OS (Windows EPP): half feedforward against a 2x plant
    // lands exactly on the command (0.5 x 2 = 1.0) — the OS can
    // never run the cursor past the hand, and there is nothing left
    // to correct.
    let mut f = PositionFollower::default();
    f.enter(100, 100);
    let mut plant = Plant::new(2.0);
    plant.real = (100.0, 100.0);
    let (dx, dy) = f.push(50, -20);
    plant.move_rel(dx, dy);
    assert_eq!(plant.position(), (150, 80), "0.5 ff x 2x gain = full frame");
    assert_eq!(f.error(plant.position()), (0, 0));
    assert_eq!(f.correct(plant.position()), None, "no residual, no correction");
}

#[test]
fn follower_recovers_from_a_lost_frame() {
    // The hand moved 40px but one frame (10px) was lost on the wire:
    // the command says 40, the real cursor only travelled 30 (1:1
    // OS). The follower must push the remaining 10px.
    let mut f = PositionFollower::default();
    f.enter(0, 0);
    let mut plant = Plant::new(1.0);
    let _ = f.push(30, 0); // received frames
    plant.move_rel(30, 0); // 30px applied; 10px lost before arrival
    let (cmd, _) = f.push(10, 0); // this frame never arrives at the OS
    let _ = cmd;
    let err = f.error(plant.position());
    assert_eq!(err, (10, 0), "command (40) minus real (30)");
    let ticks = converge(&mut f, &mut plant, 100);
    assert!(ticks < 100);
    assert_eq!(f.error(plant.position()), (0, 0), "lost motion is recovered");
}

#[test]
fn follower_has_no_direction_persistence() {
    // The queue-replay failure mode this design eliminates: after a
    // direction reversal the cursor must immediately chase the new
    // command — there is no backlog of old-direction frames to drain.
    let mut f = PositionFollower::default();
    f.enter(500, 0);
    let mut plant = Plant::new(1.0);
    plant.real = (500.0, 0.0);
    // Fast sweep left (feedforward is half a frame; the closed loop
    // delivers the rest), then an instant reversal right.
    let (dx, _) = f.push(-100, 0);
    plant.move_rel(dx, 0);
    let (dx, _) = f.push(-100, 0);
    plant.move_rel(dx, 0);
    assert_eq!(plant.position(), (400, 0));
    // Reverse: the very next injected motion must already point
    // right — no backlog of old-direction frames to drain.
    let (dx, _) = f.push(100, 0);
    plant.move_rel(dx, 0);
    let (dx, _) = f.push(100, 0);
    plant.move_rel(dx, 0);
    assert_eq!(plant.position(), (500, 0));
    // The remaining residual converges immediately.
    let ticks = converge(&mut f, &mut plant, 200);
    assert!(ticks < 200);
    assert_eq!(f.error(plant.position()), (0, 0));
}

#[test]
fn follower_survives_a_stalled_loop_without_teleporting() {
    // The loop stalled (Windows timer granularity, a slow clipboard
    // read): 40 frames of motion arrive late in one burst. The
    // corrections are capped per tick, so the cursor converges
    // smoothly instead of one giant jump — and it still catches up.
    let mut f = PositionFollower::default();
    f.enter(0, 0);
    let mut plant = Plant::new(1.0);
    for _ in 0..40 {
        let (dx, dy) = f.push(5, 0);
        plant.move_rel(dx, dy);
    }
    // But the real cursor never moved (the loop was stalled and the
    // injections above never actually reached the OS).
    plant.real = (0.0, 0.0);
    let err = f.error(plant.position());
    assert_eq!(err.0, 200);
    // First correction is bounded by the max step, not 200px.
    let (dx, _) = f.correct((0, 0)).expect("correction due");
    assert!(dx.abs() <= 32, "recovery must be capped, got {dx}");
    let ticks = converge(&mut f, &mut plant, 200);
    assert!(ticks < 200);
    assert_eq!(f.error(plant.position()), (0, 0));
}

#[test]
fn follower_leave_stops_correcting_and_flush_lands_clicks() {
    let mut f = PositionFollower::default();
    f.enter(0, 0);
    assert!(f.is_active());
    let mut plant = Plant::new(1.0);
    let _ = f.push(10, 0);
    plant.move_rel(6, 0); // OS under-applied (say, one frame lost)
    // Ordering-critical event: flush the whole residual so the click
    // lands on the command point.
    let (dx, dy) = f.flush(plant.position()).expect("residual to flush");
    plant.move_rel(dx, dy);
    assert_eq!(plant.position(), (10, 0));
    assert_eq!(f.error(plant.position()), (0, 0));
    // Leaving stops all following.
    f.leave();
    assert!(!f.is_active());
    assert_eq!(f.correct((0, 0)), None);
    assert_eq!(f.flush((0, 0)), None);
}
