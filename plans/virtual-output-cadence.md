---
status: done
---

# Pace virtual outputs like physical monitors

Tracks issue 3 on the fork (`xian0x5a/niri`): frame callbacks on a virtual output reach about twice
the refresh rate.

## Goal

A client on a virtual output sees the same frame pacing as on a physical monitor: at most one frame
callback per refresh interval, on a fixed grid, and only while its surface is shown on that output.

## Intention

Virtual outputs exist for remote streaming. A game that renders faster than the stream can show, or
keeps rendering on a hidden workspace, burns GPU that the visible stream needs. The pacing must also
be one implementation, so that the headless tests exercise what the TTY session runs.

## Scope & Constraints

- In scope: frame callback visibility, the tick grid, sharing the tick between the TTY and headless
  backends, and a rate test.
- Out of scope: capture-locked pacing, presentation feedback timestamps, and the Sunshine GPU cap
  (a degraded Sunshine process, unrelated to pacing).
- Keep rendering lazy: a tick never composites the output.
- Keep the diff in upstream-owned code small; this is a personal fork that merges upstream.
- Do not install the build or restart the running compositor.

## Work Plan

1. An independent agent writes the tests against the current code:
   - a hidden-workspace window on a virtual output must not receive frame callbacks at the refresh
     rate (fails today);
   - a client that commits on every callback receives about one callback per refresh interval. This
     replaces `virtual_output_frame_callbacks_are_paced_to_refresh`, whose check that an idle output
     answers the first commit at once moves to its own test.
2. Add `backend::virtual_output::advance`, the single tick used by both backends:
   compute the render element states, update the primary scanout output, report presentation,
   anchor the frame clock to the tick target, bump the frame callback sequence, and arm the
   estimated VBlank timer for the target.
3. Route `Tty::render` and `Headless::render` through it. Pass `target_presentation_time` to the
   headless backend.
4. Send frame callbacks with the regular `Niri::send_frame_callbacks`. Delete
   `send_frame_callbacks_for_virtual_output` and `virtual_output_has_presentation_feedbacks`.
5. Independent review, then fix findings.
6. Document the pacing contract in `docs/wiki/Virtual-Outputs.md`.

## Validation

- `cargo test -p niri --lib virtual_output frame_callbacks screencopy hidden`.
- Full workspace suite once: `cargo test --all --exclude niri-visual-tests`.
- `cargo clippy --all --all-targets`, `cargo +nightly fmt --all -- --check`.
- End to end on the headless backend: run the built binary as a second instance with a 120 Hz
  virtual output and measure with the probe from the issue. Expect about 120 callbacks per second.
- The TTY backend cannot be started in tests or next to the running session. It shares the tick
  after this change; a live check needs the user to install the build and restart the session.

## Progress

- [x] Plan written, branch `fix/virtual-output-cadence` created.
- [x] Regression tests written and seen failing for the expected reason.
- [x] Shared tick implemented, both backends routed through it.
- [x] Tests, clippy, and format pass.
- [x] Independent review done, findings addressed.
- [x] Headless end-to-end probe run.
- [x] Merged into `main` at the user's request, before the live check on the TTY backend. Issue 3
  stays open until that check.

## Decisions

### Visibility comes from render element states, on every tick

| Criterion                         | Weight | A: element states + regular callbacks | B: layout-level filter |
| --------------------------------- | ------ | ------------------------------------- | ---------------------- |
| Matches physical output behaviour | 35%    | 5                                     | 3                      |
| Fork divergence and upkeep        | 25%    | 5                                     | 2                      |
| Cost per tick                     | 20%    | 3                                     | 5                      |
| Implementation risk               | 20%    | 4                                     | 3                      |
| Weighted total                    |        | 4.40                                  | 3.15                   |

A runs the pass that presentation feedback already used (`render_to_vec` plus damage tracking, no
draw) on every tick instead of only when feedback is pending, then uses the same
`send_frame_callbacks` as physical outputs. It removes the virtual-only callback function, handles
occlusion and scrolled-out columns, and gives cursor and drag icon surfaces their callbacks.

Rejected B, a virtual-only filter on "window is on a visible workspace": it is a second visibility
mechanism in the layout, misses occlusion, and still needs its own once-per-cycle dedupe.

### The frame clock is anchored to the tick target

`frame_clock.presented(target_presentation_time)` keeps every target on one grid. Anchoring to the
redraw time made the next target fall between grid points while the armed timer still fired on the
old one, which produced two ticks per interval. Upstream PR 3800 made the same change.

### One tick, using the existing estimated VBlank timer

`queue_estimated_vblank_timer` and its handler never needed the TTY backend. The handler loses its
unused `&self` so both backends can arm it. Rejected: a separate fork-owned timer in
`virtual_output.rs`, which would keep `tty.rs` closer to upstream but copy the handler's state
machine.

### Without a renderer the tick still advances

Without a renderer there is no visibility information, so no surface counts as shown and clients
only get the 1 Hz fallback callbacks. The old code sent callbacks to everything in that case.

Rejected: returning `RenderResult::Skipped`, as a physical output does. `Niri::redraw` refuses to
lock the session when an output could not render a locked frame, so a headless session whose EGL
setup failed could no longer be locked. Nothing can be captured from such an output, which makes the
lock safe to confirm.

### The frame callback sequence is bumped in the tick

The tick counts as a submitted frame, so an idle output answers a commit at once. Without the bump
the callback would wait for the next grid point, and a client slower than the refresh rate would be
held to a fraction of that rate even when it is alone on the output. The timer bumps the sequence
again, as it does for physical outputs. A second callback within one interval would need a callback
request that queues no redraw, and every commit path queues one.

### Failure case considered

With a fixed grid, a client slower than the refresh rate drops to half rate while another client
keeps the output busy, because its commit waits for the next tick. This is what vsync does on a
physical monitor and the client can opt out by not waiting for callbacks. Before this change hidden
windows could keep the output busy as well; they no longer receive callbacks at the refresh rate, so
only visible clients can cause it.

## Surprises & Discoveries

- The headless backend's test outputs (`Fixture::add_output`) carry the virtual output marker, so
  every test output goes through the virtual tick.
- The rate test cannot be seen failing on the old code: there the 2x cadence only happens in the
  TTY path, which tests cannot start. The headless copy re-arms a full interval after each redraw
  and paces at slightly under the refresh rate. With the shared tick the test does catch it:
  anchoring the clock to the redraw time again gives 41 callbacks where 27 are allowed.
- Building the element list draws offscreen buffers for windows that are animating, so a tick is
  not free of GPU work during animations. The first such draw took about a second when four copies
  of the test suite ran at once, which made the rate test fail until its windows finished their
  open animation before measuring.
- A window cast of a hidden window on a virtual output used to animate at the full rate because of
  the unconditional callbacks. It now gets the 1 Hz fallback, as on physical outputs.

## Outcomes & Retrospective

Measured on a second, headless instance with a 120 Hz virtual output and the probe from the issue:

| Build         | Callbacks per second | Intervals (ms) | Compositor CPU |
| ------------- | -------------------- | -------------- | -------------- |
| `c98f080b`    | 119.3 to 119.6       | 8.35 to 8.38   | 1.0 to 1.3%    |
| This change   | 120.0                | 8.33 on a grid | 1.3 to 1.6%    |

The rate follows the output's mode: 60.0, 144.0 and 90.0 callbacks per second after switching the
mode to 60, 144 and 90 Hz. A hidden window gets at most the fallback callbacks, where it got 19 in
400 ms at 50 Hz before.

The independent review found no defect in the tick itself. It found the load-sensitive rate test,
the lock regression of the first no-renderer design, and wording that claimed a tick costs no GPU
time. All three are addressed.

Not yet verified on the TTY backend, the path that showed 211 callbacks per second. It now runs the
same tick as the headless backend, but a live check needs the build installed and the session
restarted.

Follow-up kept out of scope: presentation feedback on virtual outputs still reports the tick time
with an unknown refresh rate. The fixed grid would allow reporting the target time and the refresh
interval.
