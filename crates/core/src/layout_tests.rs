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
    // Exactly the geometry that broke real layouts: pc sits 2 px
    // right of where hp's edge ends (and 4 px higher). The screens
    // must still connect across both directions.
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
fn big_gap_stays_disconnected() {
    // A real gap (280 px) must not connect — dead edges stay dead
    // only when the user actually left a large hole in the layout.
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -2200, 0, 1920, 1080)]);
    assert!(l.neighbor(0, Direction::Left, 0, 300).is_none());
}

#[test]
fn partial_overlap_still_connects() {
    // hp sits higher than pc and only overlaps the top half.
    let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, -500, 1920, 1000)]);
    assert!(l.neighbor(0, Direction::Left, 0, 200).is_some());
    // No overlap at all -> not adjacent.
    let l2 = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, 1200, 1920, 1080)]);
    assert!(l2.neighbor(0, Direction::Left, 0, 200).is_none());
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
fn exit_direction_detection() {
    let l = two_screen_layout();
    assert_eq!(l.exit_direction(0, -1, 500), Some(Direction::Left));
    assert_eq!(l.exit_direction(0, 1920, 500), Some(Direction::Right));
    assert_eq!(l.exit_direction(0, 500, -1), Some(Direction::Top));
    assert_eq!(l.exit_direction(0, 500, 1080), Some(Direction::Bottom));
    assert_eq!(l.exit_direction(0, 500, 500), None);
}
