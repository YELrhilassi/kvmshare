use super::*;
use x11rb::protocol::xtest::ConnectionExt as _;

/// The decisive fact behind kernel input isolation: apps that read
/// XI2 *raw* events (browsers, smooth-scroll terminals like kitty)
/// receive them even while another client holds a core pointer grab
/// — core grabs only suppress *core* delivery. XI2 *device* grabs
/// (XIGrabDevice) are documented to report "only to the grabbing
/// client"; this test proves that **even they do not suppress raw
/// delivery** on a real server: while the grabber holds XIGrabDevice
/// on every device, an observer that selected raw events (exactly
/// like our capture does) *keeps receiving them*. That is why the
/// capture switches to kernel grabs ([`crate::evdev_reader`]) while
/// the cursor is on a client — no X-level grab can silence raw
/// readers.
#[test]
fn xi2_device_grab_does_not_suppress_raw_events_for_other_clients() {
    // Needs a live X server (real cross-client delivery rules); skip
    // silently in headless environments.
    let (Ok((observer, screen_num)), Ok((grabber, _))) = (
        RustConnection::connect(None),
        RustConnection::connect(None),
    ) else {
        eprintln!("skipping: no X server available");
        return;
    };
    let root = observer.setup().roots[screen_num].root;

    // The observer selects raw events exactly like the capture.
    select_input_events(&observer, root).expect("observer selects raw events");

    // Inject XTEST motion (produces raw events, verified below).
    let inject = |c: &RustConnection, n: i32| {
        for i in 0..n {
            let _ = c.xtest_fake_input(6, 0, x11rb::CURRENT_TIME, root, (10 + i) as i16, 10, 0);
        }
        let _ = c.flush();
    };
    fn drain_raw(c: &RustConnection, dur: Duration) -> usize {
        let deadline = Instant::now() + dur;
        let mut n = 0;
        while Instant::now() < deadline {
            match c.poll_for_event() {
                Ok(Some(XEvent::XinputRawMotion(_))) => n += 1,
                Ok(Some(_)) => {}
                Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                Err(_) => break,
            }
        }
        n
    }

    // Baseline: without a grab the observer sees the injected raw
    // motion (this also proves XTEST produces raw events at all).
    inject(&grabber, 5);
    let baseline = drain_raw(&observer, Duration::from_millis(150));
    assert!(baseline > 0, "XTEST motion should reach the observer (baseline {baseline})");

    // Grab every device with XIGrabDevice and confirm it holds
    // (grabs must succeed for the test to mean anything).
    let mask = [u32::from(XIEventMask::RAW_MOTION)];
    let devices = xinput::xi_query_device(&grabber, DEVICE_ALL).unwrap().reply().unwrap();
    let mut grabbed: Vec<u16> = Vec::new();
    for info in &devices.infos {
        let reply = xinput::xi_grab_device(
            &grabber,
            root,
            x11rb::CURRENT_TIME,
            x11rb::NONE,
            info.deviceid,
            xproto::GrabMode::ASYNC,
            xproto::GrabMode::ASYNC,
            xinput::GrabOwner::NO_OWNER,
            &mask,
        )
        .unwrap()
        .reply()
        .unwrap();
        if reply.status == xproto::GrabStatus::SUCCESS {
            grabbed.push(info.deviceid);
        }
    }
    assert!(!grabbed.is_empty(), "at least one device must be grabbed");
    let _ = grabber.flush();

    // While every device is grabbed, the observer *still* receives
    // raw motion. This is the leak this project exists to close: no
    // X-level grab — core or device — can stop raw-reading apps from
    // reacting to forwarded input. Only kernel isolation can.
    inject(&grabber, 5);
    let during = drain_raw(&observer, Duration::from_millis(150));
    assert!(
        during > 0,
        "raw motion must still reach the observer during a device grab (got {during}) — if the X server now suppresses it, revisit the isolation design"
    );

    // Release everything: raw delivery continues for the observer.
    for id in &grabbed {
        let _ = xinput::xi_ungrab_device(&grabber, x11rb::CURRENT_TIME, *id);
    }
    let _ = grabber.flush();
    inject(&grabber, 5);
    let after = drain_raw(&observer, Duration::from_millis(150));
    assert!(after > 0, "raw motion must resume after ungrab (got {after})");
}

#[test]
fn fixed_point_converts_to_f64() {
    let v = Fp3232 { integral: 3, frac: 0x8000_0000 }; // 3.5
    assert_eq!(fp_to_f64(&v), 3.5);
    let v = Fp3232 { integral: -1, frac: 0 }; // -1.0
    assert_eq!(fp_to_f64(&v), -1.0);
}

/// Fp3232 with a given whole value (no fraction).
fn fp(v: i32) -> Fp3232 {
    Fp3232 { integral: v, frac: 0 }
}

#[test]
fn raw_xy_takes_first_two_axes_positionally() {
    // Classic 2-axis mouse: mask bits 0 and 1, values in order.
    let mask = vec![0b11];
    let values = vec![fp(4), fp(-7)];
    assert_eq!(raw_xy(&mask, &values), (4.0, -7.0));
}

#[test]
fn raw_xy_ignores_scroll_axes_from_multi_valuator_devices() {
    // 4-axis mouse: RelX=0, RelY=1, RelHorizScroll=2, RelVertScroll=3.
    // A wheel notch arrives as a raw motion carrying *only* the
    // vertical scroll valuator. Positionally that value would be
    // taken as dx; mask-walking must yield zero motion.
    let mask = vec![1 << 3];
    let values = vec![fp(120)]; // one vertical notch
    assert_eq!(raw_xy(&mask, &values), (0.0, 0.0));
}

#[test]
fn raw_xy_mixes_motion_and_scroll_in_one_event() {
    // A diagonal move while the wheel ticks: all four axes present.
    // Only Rel X / Rel Y may come out; scroll deltas must be dropped.
    let mask = vec![0b1111];
    let values = vec![fp(3), fp(2), fp(-120), fp(-120)];
    assert_eq!(raw_xy(&mask, &values), (3.0, 2.0));
}

#[test]
fn raw_xy_handles_masks_spanning_multiple_words() {
    // Hypothetical device with axes beyond 32 (word 1, bit 2 = axis
    // 34). XY are still found; the extra axis contributes nothing.
    let mask = vec![1 << 0, 1 << 2];
    let values = vec![fp(9), fp(5)];
    assert_eq!(raw_xy(&mask, &values), (9.0, 0.0));
}

#[test]
fn raw_xy_with_empty_event_is_zero() {
    assert_eq!(raw_xy(&[], &[]), (0.0, 0.0));
    // Values without a mask are never indexed (mask drives the walk).
    assert_eq!(raw_xy(&[0], &[fp(99)]), (0.0, 0.0));
}
