use super::*;

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
fn follower_command_never_leaves_the_screen() {
    // The virtual cursor must respect the screen boundary: pushing
    // against an edge runs the command up to the edge and no further,
    // so a reversal moves immediately instead of walking the whole
    // off-screen overshoot back (the OS pins the visible cursor at the
    // edge meanwhile — the "stuck at the boundary" failure).
    let mut f = PositionFollower::default();
    f.set_bounds(1920, 1080);
    f.enter(100, 100);
    // Sweep far beyond the right and bottom edges: the command clamps.
    f.advance(5000, 4000);
    assert_eq!(f.command(), (1919, 1079));
    // Reversing moves immediately from the edge — no stuck cursor.
    f.advance(-10, 0);
    assert_eq!(f.command(), (1909, 1079));
    // Sweep beyond the top and left edges the same way.
    f.advance(-5000, -5000);
    assert_eq!(f.command(), (0, 0));

    // Relative backends push through the same clamp: the command stops
    // at the edge even while the feedforward injection is absorbed by
    // the OS pin.
    let mut g = PositionFollower::default();
    g.set_bounds(800, 600);
    g.enter(700, 550);
    let _ = g.push(500, 500);
    assert_eq!(g.command(), (799, 599));

    // A resolution shrink re-bounds an out-of-range command.
    let mut h = PositionFollower::default();
    h.set_bounds(1920, 1080);
    h.enter(10, 10);
    h.advance(3000, 3000);
    h.set_bounds(1280, 720);
    assert_eq!(h.command(), (1279, 719));

    // Unknown bounds leave the command free (no mis-clamp to (0,0)).
    let mut k = PositionFollower::default();
    k.enter(10, 10);
    k.advance(-50, 0);
    assert_eq!(k.command(), (-40, 10));

    // Degenerate zero-size bounds clamp to (0, 0) without panicking.
    let mut z = PositionFollower::default();
    z.set_bounds(0, 0);
    z.enter(5, 5);
    z.advance(10, 10);
    assert_eq!(z.command(), (0, 0));
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