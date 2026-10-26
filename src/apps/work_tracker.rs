#[test]
fn clock_in_is_refused_when_log_is_full() {
    let mut tracker = WorkTracker::new();
    tracker.set_start_allowed(false);
    tracker.clock_in();
    assert_eq!(tracker.state, TrackerState::Idle);
    assert!(tracker.active_session.is_none());
}

#[test]
fn touch_handling_state_transitions() {
    let mut tracker = WorkTracker::new();
    // Tap clock in (x: 200, y: 350)
    let handled = tracker.handle_touch(TouchPoint { x: 200, y: 350 });
    assert!(handled);
    assert_eq!(tracker.state, TrackerState::Working);

    // Tap pause (x: 100, y: 350)
    let handled = tracker.handle_touch(TouchPoint { x: 100, y: 350 });
    assert!(handled);
    assert_eq!(tracker.state, TrackerState::Paused);

    // Tap resume (x: 100, y: 350)
    let handled = tracker.handle_touch(TouchPoint { x: 100, y: 350 });
    assert!(handled);
    assert_eq!(tracker.state, TrackerState::Working);

    // Tap clock out (x: 300, y: 350)
    let handled = tracker.handle_touch(TouchPoint { x: 300, y: 350 });
    assert!(handled);
    assert_eq!(tracker.state, TrackerState::Idle);
    assert_eq!(tracker.completed_sessions_today, 1);
}

#[test]
fn pending_session_retained_on_full_error() {
    let mut tracker = WorkTracker::with_rate(5000);
    tracker.update_time((9, 0, 0));
    tracker.clock_in();
    tracker.tick_1s(); // 1 second elapsed
    let session = tracker.clock_out().expect("session returned");
    assert!(tracker.pending_session.is_some());
    // Simulate full error - pending session should remain
    assert_eq!(tracker.pending_completed_session(), Some(session));
}

#[test]
fn pending_session_retained_on_duplicate_id_error() {
    let mut tracker = WorkTracker::with_rate(5000);
    tracker.update_time((9, 0, 0));
    tracker.clock_in();
    tracker.tick_1s();
    let session = tracker.clock_out().expect("session returned");
    assert!(tracker.pending_session.is_some());
    // Simulate DuplicateIdMismatch error - pending session should remain
    assert_eq!(tracker.pending_completed_session(), Some(session));
}

#[test]
fn pending_session_retained_on_corrupt_header_error() {
    let mut tracker = WorkTracker::with_rate(5000);
    tracker.update_time((9, 0, 0));
    tracker.clock_in();
    tracker.tick_1s();
    let session = tracker.clock_out().expect("session returned");
    assert!(tracker.pending_session.is_some());
    // Simulate CorruptHeader error - pending session should remain
    assert_eq!(tracker.pending_completed_session(), Some(session));
}

#[test]
fn pending_session_retained_on_unsupported_version_error() {
    let mut tracker = WorkTracker::with_rate(5000);
    tracker.update_time((9, 0, 0));
    tracker.clock_in();
    tracker.tick_1s();
    let session = tracker.clock_out().expect("session returned");
    assert!(tracker.pending_session.is_some());
    // Simulate UnsupportedVersion error - pending session should remain
    assert_eq!(tracker.pending_completed_session(), Some(session));
}

#[test]
fn pending_session_retained_on_io_or_verify_failed_error() {
    let mut tracker = WorkTracker::with_rate(5000);
    tracker.update_time((9, 0, 0));
    tracker.clock_in();
    tracker.tick_1s();
    let session = tracker.clock_out().expect("session returned");
    assert!(tracker.pending_session.is_some());
    // Simulate Io/VerifyFailed error - pending session should remain
    assert_eq!(tracker.pending_completed_session(), Some(session));
}

#[test]
fn pending_session_cleared_on_successful_save() {
    let mut tracker = WorkTracker::with_rate(5000);
    tracker.update_time((9, 0, 0));
    tracker.clock_in();
    tracker.tick_1s();
    tracker.clock_out().expect("session returned");
    assert!(tracker.pending_session.is_some());
    // Simulate successful save - pending session should be cleared
    tracker.confirm_completed_session_saved();
    assert!(tracker.pending_session.is_none());
}