---
status: done
---

# Report presentation feedback on virtual outputs like on physical monitors

Tracks issue 4 on the fork (`xian0x5a/niri`): presentation feedback on a virtual output reports the
tick time, an unknown refresh rate and a sequence of 0.

## Goal

A client on a virtual output receives presentation feedback that describes the refresh cycle its
frame was shown in, as on a physical monitor: the event arrives at the estimated VBlank of the tick
target, its timestamp is that target, and it carries the output's refresh interval.

## Intention

Virtual outputs exist for remote streaming, often of games. Clients use these fields for pacing:

- Mesa's Vulkan WSI completes present waits on `presented` and feeds `refresh` into present timing
  and FIFO pacing. An early event completes the wait before the frame's refresh cycle.
- Chromium only adopts a refresh interval from feedback, and re-anchors its begin-frame timebase only
  when the `vsync` kind flag is set. With an unknown refresh it keeps its 60 Hz default.
- mpv reads `refresh` as its vsync interval.

## Scope & Constraints

- In scope: timestamp, refresh, kind flags, and when the event is sent, for virtual outputs on both
  backends.
- Out of scope: the sequence counter (see Decisions), physical outputs, the winit backend.
- Keep the diff in upstream-owned code small; this is a personal fork that merges upstream.
- Do not install the build or restart the running compositor.

## Work Plan

1. An independent agent extends the test client to record the `presented` fields and writes a test
   against the current code; it must fail for the expected reason.
2. `virtual_output::advance` stores the tick's feedback with its target in `OutputState` instead of
   reporting it at once.
3. The estimated VBlank timer handler reports the stored feedback: timestamp = target, refresh =
   `Fixed(interval)`, kind = `Vsync`, sequence 0.
4. Independent review, then fix findings.
5. Document the feedback contract in `docs/wiki/Virtual-Outputs.md`.

## Validation

- `cargo test -p niri --lib virtual_output`.
- Full workspace suite once: `cargo test --all --exclude niri-visual-tests`.
- `cargo clippy --all --all-targets`, `cargo +nightly fmt --all -- --check`.

## Progress

- [x] Plan written, branch `fix/virtual-output-presentation-feedback` created.
- [x] Regression test written and seen failing for the expected reason (refresh 0).
- [x] Fix implemented.
- [x] Tests, clippy, and format pass.
- [x] Independent review done, findings addressed.
- [x] Docs updated, plan closed.
- [x] Live check on the TTY backend passed after the user installed the build and restarted the
  session. Issue 4 closed.

## Decisions

### Send the feedback from the estimated VBlank timer

The tick latches the client buffers; the timer stands in for the VBlank at which a physical output
would show them. Sending at the timer makes the timestamp a time that has passed, as the protocol's
"was displayed" requires, and keeps the event order of a physical output: `presented` before the
next frame callbacks.

Rejected: reporting the target at once from the tick. The target is up to one interval in the
future, which completes Mesa's present waits early.

The redraw of a virtual output only runs in the `Queued` state, so at most one tick is pending when
the timer fires. The timer is only removed with the output; dropping `OutputState` then drops the
feedback, which smithay reports as discarded.

### Timestamp is the tick target

Targets sit on the frame clock's grid, so consecutive timestamps differ by whole intervals and a
client that estimates the refresh from them gets the exact value. The timer's own wake-up time
carries event-loop jitter.

### A target that has passed moves to the next grid point

The tick's target has already passed on the first tick of a new frame clock, which has no grid yet
and returns the current time, and after a tick slower than the time left before its target. The
timer then fires one interval late, so the report would claim a presentation at the time of the
latch and stretch the gap to the next frame. `advance` moves such a target to the next point of the
grid before it stores the feedback, anchors the clock and arms the timer, so the timestamp is always
the timer's deadline. The refresh interval is captured at the tick as well, so a mode switch before
the timer fires does not relabel the frame.

### Kind is `Vsync`

The content cannot tear and changes only on the refresh grid. Chromium needs the flag to follow the
grid. `HwClock`, `HwCompletion` and `ZeroCopy` claim hardware behaviour a virtual output lacks.

### Sequence stays 0

KWin reports 0 on every output. mpv, the one client found that reads it, treats 0 as unknown and
falls back to the output's refresh rate. A count derived from the timestamp would run backwards when
the refresh rate drops in a mode switch, and a count of ticks would skip idle cycles.

## Surprises & Discoveries

- The headless test fixture already creates a renderer, so the old presence-only feedback test and
  the helper's explicit `add_renderer` both run with one. The new test replaces the old one.
- The first tick after every mode switch, output creation or off/on reported a timestamp one
  interval before the event arrived. The review found it; a probe measured 34.6 ms at 30 Hz and
  9.4 ms at 120 Hz before the grid fix, 1.3 ms and 0.25 ms after.
- clippy 1.100 warns about `if` guards returning bool literals in four places this change does not
  touch.

## Outcomes & Retrospective

Feedback on a virtual output now arrives at the estimated VBlank of the tick target, carries that
target as its timestamp, the output's refresh interval and the `Vsync` flag. The regression test
`virtual_output_presentation_feedback_describes_the_refresh_cycle` failed on the old code with
refresh 0, and also catches the rejected design of reporting the target at once (timestamp after
receipt). The first-tick case has no committed test: its observable effect is latency, which cannot
be bounded reliably on a loaded machine, so it was checked with a throwaway probe.

On the TTY backend, a probe on the live session's 120 Hz virtual output received feedback for every
frame and none discarded. Every feedback carried a refresh of 8333333 ns, the `Vsync` flag and
sequence 0; the timestamps sat exactly on one grid, one interval apart, and each feedback arrived
0.2 to 0.6 ms after its timestamp.
