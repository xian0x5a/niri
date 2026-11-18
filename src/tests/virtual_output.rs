use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use smithay::reexports::wayland_protocols::wp::presentation_time::client::wp_presentation_feedback;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::{ClientId, PresentationFeedbackData};
use super::*;
use crate::niri::RedrawState;

/// Sets up a fixture with a virtual output named `virt` at the given refresh rate.
fn fixture_with_virtual_output(refresh_hz: u32) -> Fixture {
    let mut f = Fixture::new();
    // Frame callbacks only go to surfaces the output shows, and working that out takes a renderer.
    f.niri_state().backend.headless().add_renderer().unwrap();

    let state = f.niri_state();
    state
        .backend
        .create_virtual_output(
            &mut state.niri,
            1920,
            1080,
            refresh_hz,
            Some("virt".to_owned()),
        )
        .unwrap();
    f
}

/// Maps a window with a buffer on the active workspace and returns its surface.
fn map_window(f: &mut Fixture, id: ClientId) -> WlSurface {
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    // An unfinished open animation is drawn through an offscreen buffer on every refresh cycle,
    // and the first such draw can take a second on a loaded machine.
    f.niri_complete_animations();
    f.double_roundtrip(id);
    surface
}

/// Drives windows like a game for `duration` and returns when each received its frame callbacks.
///
/// Every window commits with a new frame callback request, and again as soon as the previous
/// callback is done. The fixture's event loop is manual, so it is pumped on every iteration.
fn frame_callback_times_of_busy_windows(
    f: &mut Fixture,
    id: ClientId,
    surfaces: &[&WlSurface],
    duration: Duration,
) -> Vec<Vec<Duration>> {
    let mut times = vec![Vec::new(); surfaces.len()];
    let mut pending: Vec<_> = surfaces
        .iter()
        .map(|surface| {
            let window = f.client(id).window(surface);
            let callback = window.request_frame_callback();
            window.commit();
            callback
        })
        .collect();

    let start = Instant::now();
    while start.elapsed() < duration {
        std::thread::sleep(Duration::from_millis(1));
        f.roundtrip(id);

        for ((surface, times), callback) in surfaces.iter().zip(&mut times).zip(&mut pending) {
            if callback.done.load(Ordering::Relaxed) {
                times.push(start.elapsed());
                let window = f.client(id).window(surface);
                *callback = window.request_frame_callback();
                window.commit();
            }
        }
    }
    times
}

#[test]
fn virtual_output_does_not_pace_hidden_windows_at_refresh_rate() {
    const REFRESH_HZ: u32 = 50;
    let duration = Duration::from_millis(400);
    let expected = duration.as_secs_f64() * f64::from(REFRESH_HZ);

    let mut f = fixture_with_virtual_output(REFRESH_HZ);
    let id = f.add_client();

    // The first window goes to the workspace below without following, then a second window maps
    // on the still active workspace.
    let hidden = map_window(&mut f, id);
    f.niri().layout.move_to_workspace_down(false);
    f.niri_complete_animations();
    let visible = map_window(&mut f, id);

    // Sanity: only the second window is on the shown workspace.
    let shown_windows = f
        .niri()
        .layout
        .active_workspace()
        .unwrap()
        .windows()
        .count();
    assert_eq!(shown_windows, 1);
    assert_eq!(f.niri().layout.windows().count(), 2);

    let times = frame_callback_times_of_busy_windows(&mut f, id, &[&visible, &hidden], duration);
    let (visible_count, hidden_count) = (times[0].len(), times[1].len());

    assert!(
        visible_count as f64 >= 0.6 * expected,
        "visible window got {visible_count} callbacks, expected about {expected}"
    );
    // A hidden surface only gets the fallback callbacks, about one per second.
    assert!(
        hidden_count <= 2,
        "hidden window got {hidden_count} callbacks in {duration:?} (visible got {visible_count})"
    );
}

#[test]
fn virtual_output_frame_callbacks_are_at_most_one_per_refresh_interval() {
    const REFRESH_HZ: u32 = 25;
    let duration = Duration::from_secs(1);
    let interval = Duration::from_secs(1) / REFRESH_HZ;
    let expected = duration.as_secs_f64() * f64::from(REFRESH_HZ);

    let mut f = fixture_with_virtual_output(REFRESH_HZ);
    let id = f.add_client();
    let surface = map_window(&mut f, id);

    let times = frame_callback_times_of_busy_windows(&mut f, id, &[&surface], duration).remove(0);
    let count = times.len();

    // A stalled test thread can only lower the count, so this bound holds on a loaded machine.
    assert!(
        count as f64 <= expected + 2.0,
        "got {count} callbacks, at most one per {interval:?} (about {expected}) allowed"
    );
    assert!(
        count as f64 >= 0.4 * expected,
        "got {count} callbacks, expected about {expected}"
    );

    // The count alone would accept a steady half rate. The median gap is not moved by a few
    // stalls, unlike the count.
    let mut gaps: Vec<_> = times.windows(2).map(|pair| pair[1] - pair[0]).collect();
    gaps.sort();
    let median_gap = gaps[gaps.len() / 2];
    assert!(
        interval / 2 <= median_gap && median_gap <= interval * 3 / 2,
        "median gap between callbacks is {median_gap:?}, expected about {interval:?}"
    );
}

#[test]
fn virtual_output_sends_frame_callback_at_once_when_idle() {
    // A long refresh interval tells "at once" apart from "at the next refresh".
    let mut f = fixture_with_virtual_output(4);
    let id = f.add_client();
    let surface = map_window(&mut f, id);

    // Let the refresh cycles that mapping started run out.
    let deadline = Instant::now() + Duration::from_secs(5);
    while f
        .niri()
        .output_state
        .values()
        .any(|state| !matches!(state.redraw_state, RedrawState::Idle))
    {
        assert!(Instant::now() < deadline, "output did not become idle");
        std::thread::sleep(Duration::from_millis(5));
        f.roundtrip(id);
    }

    let window = f.client(id).window(&surface);
    let callback = window.request_frame_callback();
    window.commit();
    f.double_roundtrip(id);

    // An idle output starts a refresh cycle right away, as a physical output does. Waiting for the
    // next point of the refresh grid instead would hold a client that renders slower than the
    // refresh rate to a fraction of that rate.
    assert!(callback.done.load(Ordering::Relaxed));
}

#[test]
fn virtual_output_custom_mode_does_not_accumulate_modes() {
    let mut f = Fixture::new();

    // Create a managed virtual output so it goes through the same config application path as in a
    // real session (`niri msg create-virtual-output`, `niri msg output ... custom-mode`).
    let name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some("sunshine".to_owned()))
            .unwrap()
    };

    let output = f
        .niri()
        .global_space
        .outputs()
        .find(|o| o.name() == name)
        .unwrap()
        .clone();

    // Sanity: single initial mode.
    {
        let modes = output.modes();
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].size.w, 1920);
        assert_eq!(modes[0].size.h, 1080);
    }

    // 1080p -> 3200x1800
    {
        let state = f.niri_state();
        state.apply_transient_output_config(
            &name,
            niri_ipc::OutputAction::CustomMode {
                mode: niri_ipc::ConfiguredMode {
                    width: 3200,
                    height: 1800,
                    refresh: Some(60.0),
                },
            },
        );
    }

    {
        let modes = output.modes();
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].size.w, 3200);
        assert_eq!(modes[0].size.h, 1800);
    }

    // 3200x1800 -> 1080p
    {
        let state = f.niri_state();
        state.apply_transient_output_config(
            &name,
            niri_ipc::OutputAction::CustomMode {
                mode: niri_ipc::ConfiguredMode {
                    width: 1920,
                    height: 1080,
                    refresh: Some(60.0),
                },
            },
        );
    }

    {
        let modes = output.modes();
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].size.w, 1920);
        assert_eq!(modes[0].size.h, 1080);
    }
}

#[test]
fn off_virtual_output_can_be_removed() {
    let mut f = Fixture::new();

    let name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some("virt".to_owned()))
            .unwrap()
    };

    f.niri_state()
        .apply_transient_output_config(&name, niri_ipc::OutputAction::Off);

    {
        let state = f.niri_state();
        state
            .backend
            .remove_virtual_output(&mut state.niri, &name)
            .unwrap();
    }

    let state = f.niri_state();
    assert!(state
        .backend
        .remove_virtual_output(&mut state.niri, &name)
        .is_err());
}

#[test]
fn configured_virtual_output_cannot_be_removed_at_runtime() {
    let mut f = Fixture::new();
    let name = "virt";

    {
        let state = f.niri_state();
        state.modify_output_config(name, |config| config.create_virtual = true);
        state.reload_output_config();
    }

    let result = {
        let state = f.niri_state();
        state.backend.remove_virtual_output(&mut state.niri, name)
    };

    assert!(result.is_err_and(|err| err.contains("is configured")));
    assert!(f
        .niri_state()
        .backend
        .ipc_outputs()
        .lock()
        .unwrap()
        .values()
        .any(|output| output.name == name));
}

#[test]
fn virtual_output_name_is_reported_as_its_model() {
    let mut f = Fixture::new();
    let name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some("sunshine".to_owned()))
            .unwrap()
    };

    let ipc_outputs = f.niri_state().backend.ipc_outputs();
    let ipc_outputs = ipc_outputs.lock().unwrap();
    let output = ipc_outputs
        .values()
        .find(|output| output.name == name)
        .unwrap();

    assert_eq!(output.model, name);
}

#[test]
fn touch_input_targets_virtual_output_when_focused() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    // Create a virtual output and focus it.
    let name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some("virt".to_owned()))
            .unwrap()
    };

    let virt = f
        .niri()
        .global_space
        .outputs()
        .find(|o| o.name() == name)
        .unwrap()
        .clone();

    f.niri().layout.focus_output(&virt);

    // With no explicit `input.touch.map-to-output` configured, touch should follow the active
    // output (which may be virtual).
    let touch_output = f.niri().output_for_touch().unwrap().clone();
    assert_eq!(touch_output, virt);
}

/// Commits `surface` with a presentation feedback request and pumps the event loop until the
/// compositor answers. The estimated VBlank timer only fires while the loop is dispatched.
fn wait_for_presentation_feedback(
    f: &mut Fixture,
    id: ClientId,
    surface: &WlSurface,
) -> PresentationFeedbackData {
    let feedback = f.client(id).request_presentation_feedback(surface);
    f.client(id).window(surface).commit();

    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(1));
        f.roundtrip(id);

        let data = feedback.data.lock().unwrap();
        assert!(!data.discarded, "presentation feedback was discarded");
        if data.presented {
            return data.clone();
        }
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "no presentation feedback within 1 s"
        );
    }
}

#[test]
fn virtual_output_presentation_feedback_describes_the_refresh_cycle() {
    const REFRESH_HZ: u32 = 60;
    // Same integer division as the output's refresh interval (1e12 / millihertz).
    let refresh_ns = 1_000_000_000_000_u64 / (u64::from(REFRESH_HZ) * 1000);

    let mut f = fixture_with_virtual_output(REFRESH_HZ);
    let id = f.add_client();
    let surface = map_window(&mut f, id);

    let frames: Vec<_> = (0..3)
        .map(|_| wait_for_presentation_feedback(&mut f, id, &surface))
        .collect();

    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(
            u64::from(frame.refresh_ns),
            refresh_ns,
            "frame {i}: refresh must be the output's refresh interval in ns"
        );
        assert!(
            frame
                .flags
                .is_some_and(|flags| flags.contains(wp_presentation_feedback::Kind::Vsync)),
            "frame {i}: flags {:?} must contain Vsync",
            frame.flags
        );
        assert!(
            frame.timestamp <= frame.received_at,
            "frame {i}: timestamp {:?} is after receipt at {:?}",
            frame.timestamp,
            frame.received_at
        );
    }

    // Frames shown in different refresh cycles sit a whole number of intervals apart.
    for (i, pair) in frames.windows(2).enumerate() {
        let delta_ns = pair[1]
            .timestamp
            .saturating_sub(pair[0].timestamp)
            .as_nanos();
        assert!(
            delta_ns > 0 && delta_ns % u128::from(refresh_ns) == 0,
            "frames {i} and {}: timestamps differ by {delta_ns} ns, not a nonzero multiple of \
             {refresh_ns} ns",
            i + 1
        );
    }
}
