use super::*;
use kvmshare_protocol::message::Rect;

fn screen(id: u8, x: i32, y: i32, w: i32, h: i32) -> Screen {
    Screen { id, name: id.to_string(), rect: Rect { x, y, w, h } }
}

/// Classic deskflow setup: pc (server) on the right, hp to its left.
fn two_screen_layout() -> Layout {
    Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, 0, 1920, 1080)])
}

#[test]
fn screen_at_picks_the_right_screen() {
    let l = two_screen_layout();
    assert_eq!(l.screen_at(100, 100).unwrap().id, 0);
    assert_eq!(l.screen_at(-100, 100).unwrap().id, 1);
    assert!(l.screen_at(9999, 9999).is_none());
}

#[test]
fn left_neighbor_is_hp() {
    let l = two_screen_layout();
    let (id, x, y) = l.neighbor(0, Direction::Left, 0, 300).unwrap();
    assert_eq!(id, 1);
    assert_eq!(x, 1919); // hp's right edge
    assert_eq!(y, 300); // vertical offset preserved
}

#[test]
fn right_neighbor_is_pc() {
    let l = two_screen_layout();
    let (id, x, y) = l.neighbor(1, Direction::Right, 0, 400).unwrap();
    assert_eq!(id, 0);
    assert_eq!(x, 0); // pc's left edge
    assert_eq!(y, 400);
}

#[test]
fn no_neighbor_on_outer_edge() {
    let l = two_screen_layout();
    assert!(l.neighbor(1, Direction::Left, 0, 0).is_none());
    assert!(l.neighbor(0, Direction::Right, 0, 0).is_none());
}

#[test]
fn entry_y_is_clamped_into_neighbor() {
    let l = two_screen_layout();
    let (_, _, y) = l.neighbor(0, Direction::Left, 0, 99999).unwrap();
    assert_eq!(y, 1079);
}

#[test]
fn small_gap_still_connects() {
    // pc sits 2 px right of where hp's edge ends (and 4 px higher).
    // The screens must connect across both directions.
    let l = Layout::new(vec![screen(0, 2, 0, 1920, 1080), screen(1, -1920, -4, 1920, 1080)]);
    let (id, x, y) = l.neighbor(0, Direction::Left, 2, 300).unwrap();
    assert_eq!(id, 1);
    assert_eq!(x, 1919); // hp's right edge
    assert_eq!(y, 304); // hp is 4 px higher, so pc row 300 maps to hp row 304
    // And back: hp -> pc (virtual row 300 maps straight across).
    let (id, x, y) = l.neighbor(1, Direction::Right, -1, 300).unwrap();
    assert_eq!(id, 0);
    assert_eq!(x, 0); // pc's left edge
    assert_eq!(y, 300);
}

#[test]
fn big_gap_still_crosses() {
    // A deliberate 280 px gap is crossed like contact: the ray cast
    // flies over the hole and lands on the only screen in that
    // direction. (The old edge-contact model made this a dead edge.)
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -2200, 0, 1920, 1080)]);
    let (id, x, y) = l.neighbor(0, Direction::Left, 0, 300).unwrap();
    assert_eq!(id, 1);
    assert_eq!(x, 1919);
    assert_eq!(y, 300);
    // And back.
    assert!(l.neighbor(1, Direction::Right, 0, 300).is_some());
}

#[test]
fn ray_picks_the_nearest_screen_across_a_gap() {
    // Two screens left of the server, a gap between them: the ray must
    // land on the nearer one (B), not the farther (A).
    let l = Layout::new(vec![
        screen(0, 0, 0, 1920, 1080),
        screen(2, -2100, 0, 1920, 1080), // B: nearer
        screen(1, -4100, 0, 1920, 1080), // A: farther
    ]);
    let (id, x, y) = l.neighbor(0, Direction::Left, 0, 500).unwrap();
    assert_eq!(id, 2);
    assert_eq!(x, 1919);
    assert_eq!(y, 500);
}

#[test]
fn exit_point_outside_a_smaller_neighbor_lands_at_its_nearest_point() {
    // The neighbor is half-height and sits above the exit row: the
    // entry clamps to its nearest corner instead of failing.
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, -1000, 1920, 500)]);
    let (id, x, y) = l.neighbor(0, Direction::Left, 0, 900).unwrap();
    assert_eq!(id, 1);
    assert_eq!(x, 1919);
    assert_eq!(y, 499); // clamped into hp's span
}

#[test]
fn partial_overlap_still_connects() {
    // hp sits higher than pc and only overlaps the top half.
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, -500, 1920, 1000)]);
    assert!(l.neighbor(0, Direction::Left, 0, 200).is_some());
    // No overlap at all -> the exit point clamps to the nearest corner,
    // but the screen is still there (the ray finds it past the edge).
    let l2 = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, 1200, 1920, 1080)]);
    let (id, _, _) = l2.neighbor(0, Direction::Left, 0, 200).unwrap();
    assert_eq!(id, 1);
}

#[test]
fn stacked_screens_connect_vertically() {
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, 0, -1080, 1920, 1080)]);
    let (id, x, y) = l.neighbor(0, Direction::Top, 500, 0).unwrap();
    assert_eq!(id, 1);
    assert_eq!(x, 500);
    assert_eq!(y, 1079);
}

#[test]
fn vertically_offset_screens_cross_without_a_span() {
    // Diagonal arrangement: B sits above-right of A with no vertical
    // span in common. Exiting A leftward at a row past B's span still
    // reaches B (clamped to its corner) — the placement means "B is to
    // the left".
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -2000, -1200, 1920, 1080)]);
    let (id, x, y) = l.neighbor(0, Direction::Left, 0, 1079).unwrap();
    assert_eq!(id, 1);
    assert_eq!(x, 1919);
    assert_eq!(y, 1079); // clamped into B's bottom edge region
}

#[test]
fn screen_sides_that_face_away_are_not_neighbors() {
    // A screen directly *above* the exit row is not a left-neighbor.
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, 0, -1080, 1920, 1080)]);
    assert!(l.neighbor(0, Direction::Left, 0, 500).is_none());
}

#[test]
fn exit_direction_detection() {
    let l = two_screen_layout();
    assert_eq!(l.exit_direction(0, -1, 500), Some(Direction::Left));
    assert_eq!(l.exit_direction(0, 1920, 500), Some(Direction::Right));
    assert_eq!(l.exit_direction(0, 500, -1), Some(Direction::Top));
    assert_eq!(l.exit_direction(0, 500, 1080), Some(Direction::Bottom));
    assert_eq!(l.exit_direction(0, 500, 500), None);
}

#[test]
fn issues_report_overlap() {
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -100, 0, 1920, 1080)]);
    let issues = l.issues();
    assert_eq!(issues.len(), 1);
    assert!(issues[0].contains("overlap"));
}

#[test]
fn issues_empty_for_a_clean_layout() {
    let l = two_screen_layout();
    assert!(l.issues().is_empty());
}

#[test]
fn issues_report_degenerate_size() {
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, 0, 0, 1080)]);
    let issues = l.issues();
    assert!(issues.iter().any(|i| i.contains("no size")));
}
