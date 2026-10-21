//! Stroke engine — port of libmypaint's `mypaint_brush_stroke_to`.
//!
//! Pipeline per event:
//! 1. If the stroke is fresh (or `dtime > 5 s`), seed state, request a
//!    brushwork split (libmypaint returns TRUE) and draw nothing.
//! 2. Compute raw event delta and tilt-derived inputs.
//! 3. Apply `slow_tracking` to advance `state.actual_x/y` toward the event.
//! 4. Run a libmypaint-style `while (dabs_moved + dabs_todo >= 1)` loop.
//!    Each iteration advances `cur_pressure`, `state.norm_speedN_slow`,
//!    `cur_ax/cur_ay` (lagged dab centre via `slow_tracking_per_dab`) and
//!    `state.stroke_state`, re-evaluates every setting, then draws the
//!    dab. `random_input` is refreshed from the PRNG after each draw.
//! 5. A final no-draw step absorbs the remaining `dtime_left` into the
//!    speed slowness state so the next event starts cleanly.
//!
//! The spectral `paint` path, smudge buckets and the Surface2 colour
//! sampling (`get_color_pigment`) are implemented; use
//! [`Brush::stroke_to_full`] with `linear = true` to match the MyPaint
//! application.

use crate::brush::Brush;
use crate::color::{hsv_to_rgb, Hsv, RgbaF32};
use crate::evaluator::{evaluate, InputValues, SettingValues};
use crate::setting::BrushSetting;
use crate::state::{BrushState, SmudgeBucket};
use crate::surface::{Dab, TiledSurface};
use crate::BrushInput;

impl Brush {
    /// Compatibility wrapper for callers that only have the classic six
    /// pointer axes. Equivalent to 100% zoom, no view rotation, no barrel
    /// rotation, and non-linear (sRGB) colour dynamics.
    ///
    /// Returns `true` if at least one dab was painted.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_to<S: TiledSurface>(
        &self,
        state: &mut BrushState,
        surface: &mut S,
        x: f32,
        y: f32,
        pressure: f32,
        xtilt: f32,
        ytilt: f32,
        dtime: f64,
    ) -> bool {
        self.stroke_to_full(
            state, surface, x, y, pressure, xtilt, ytilt, dtime, 1.0, 0.0, 0.0, false,
        )
    }

    /// Feed one pointer event with libmypaint's full axis set.
    ///
    /// `viewzoom` is a factor (1.0 = 100%), `viewrotation` is radians, and
    /// `barrel_rotation` is a fraction of a turn (0.0..1.0), matching
    /// `mypaint_brush_stroke_to()`. `linear` selects the linear-sRGB colour
    /// dynamics path (MyPaint passes `eotf() != 1.0`, default `true`).
    ///
    /// Returns `true` if at least one dab was painted. The split request is
    /// published separately through [`BrushState::split_due`].
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_to_full<S: TiledSurface>(
        &self,
        state: &mut BrushState,
        surface: &mut S,
        x: f32,
        y: f32,
        pressure: f32,
        xtilt: f32,
        ytilt: f32,
        dtime: f64,
        viewzoom: f32,
        viewrotation: f32,
        barrel_rotation: f32,
        linear: bool,
    ) -> bool {
        // libmypaint only floors pressure at zero (`if (pressure <= 0.0)
        // pressure = 0.0`, mypaint-brush.c:1328); it deliberately allows
        // pressure > 1.0 through the engine. The GUI clamps to [0, 1]
        // before calling in (`freehand.py:598`).
        let pressure = if pressure <= 0.0 { 0.0 } else { pressure };
        // libmypaint clamps tilt to [-1, 1] before deriving any inputs
        // (mypaint-brush.c:1310-1314).
        let xtilt = xtilt.clamp(-1.0, 1.0);
        let ytilt = ytilt.clamp(-1.0, 1.0);
        // libmypaint clamps non-positive dtime to 0.0001 *before* the skip
        // accumulation and reset check (mypaint-brush.c:1341).
        let mut dtime = if dtime <= 0.0 { 0.0001 } else { dtime };
        let viewzoom = viewzoom.max(1e-6);
        let viewrotation_deg = mod_arith_c((degrees_c(viewrotation) + 180.0) as f32, 360.0) - 180.0;
        // libmypaint only interpolates the barrel axis; zoom and view
        // rotation are event-level values copied into STATE each step.
        state.viewzoom = viewzoom;
        state.viewrotation = viewrotation_deg;
        state.split_due = false;

        // Tablets that do not report motion events without pressure can make
        // the first event after a pause interpolate pressure from the previous
        // stroke. libmypaint pre-rolls a zero-pressure event
        // (mypaint-brush.c:1346-1351). Keep the exact 0.1 s threshold.
        if dtime > 0.100 && pressure > 0.0 && state.pressure == 0.0 {
            let _ = self.stroke_to_full(
                state,
                surface,
                x,
                y,
                0.0,
                90.0,
                0.0,
                dtime - 0.0001,
                viewzoom,
                viewrotation,
                // libmypaint's synthetic warm-up event passes 0.0 for the
                // barrel axis, not the current event's value.
                0.0,
                linear,
            );
            // The synthetic event's finished/empty return is discarded by
            // libmypaint; do not let its reset request split this event.
            state.split_due = false;
            dtime = 0.0001;
        }

        // Capture the pressure value from the previous event *before* we
        // overwrite it: dabs emitted inside this stroke segment interpolate
        // pressure linearly along the segment, the way libmypaint advances
        // STATE.PRESSURE inside its `while (dabs_moved + dabs_todo >= 1.0)`
        // loop. Without this carry, every dab uses the event's final pressure
        // and pressure-driven dynamics (radius, opacity, …) jump in steps.
        let entry_pressure = if state.started {
            state.last_pressure
        } else {
            pressure
        };
        state.last_pressure = pressure;

        // --- Tracking-noise skip window --------------------------------------
        // libmypaint runs this block FIRST, before the noise / slow-tracking /
        // reset logic (mypaint-brush.c:1352-1368): while the cursor hasn't
        // travelled `0.5 * noise` pixels since the window was armed, events
        // are dropped wholesale (no RNG, no state advance) and their dtime
        // accumulates. When the window resolves, processing continues with
        // the *accumulated* dtime as if one large event had arrived.
        if state.skip_distance > 0.001 {
            let dx_skip = state.skip_last_x - x;
            let dy_skip = state.skip_last_y - y;
            let dist = dx_skip.hypot(dy_skip);
            state.skip_last_x = x;
            state.skip_last_y = y;
            // libmypaint stores `skipped_dtime` as a float and adds the
            // double event dt into it; keep the same narrowing per event.
            state.skipped_dtime += dtime as f32;
            state.skip_distance -= dist;
            dtime = state.skipped_dtime as f64;

            if state.skip_distance > 0.001 && dtime <= 5.0 {
                state.last_pressure = entry_pressure; // restore — event dropped
                return false;
            }

            state.skip_distance = 0.0;
            state.skip_last_x = 0.0;
            state.skip_last_y = 0.0;
            state.skipped_dtime = 0.0;
        }

        // --- Fresh stroke: seed state, no dabs ------------------------------
        // libmypaint resets on `dtime > max_dtime` (strictly greater;
        // mypaint-brush.c:1398), so exactly 5.0 s is still one stroke.
        if !state.started || dtime > 5.0 {
            // libmypaint's stroke_to_internal runs the tracking_noise block
            // (mypaint-brush.c:1373-1389) BEFORE the `dtime > max_dtime`
            // brush_reset path (line 1396). When TRACKING_NOISE is set
            // that means each fresh-stroke / warm-up event consumes 2
            // `rand_gauss` draws (= 8 `rng_double_next` units) AND folds
            // the noisy x/y into STATE.X/Y via the brush_reset's explicit
            // `STATE.X = x; STATE.Y = y;` assignment at line 1404-1405
            // (after the reset's memset). hokusai used to skip straight
            // to the reset path with the raw `x, y`, losing both the RNG
            // sequence offset AND the per-stroke initial noise vector.
            let base_radius_init = self
                .get(BrushSetting::Radius)
                .base_value
                .exp();
            let noise_init = base_radius_init * self.get(BrushSetting::TrackingNoise).base_value;
            let (mut seed_x, mut seed_y) = (x, y);
            if noise_init > 0.001 {
                seed_x += state.rng.next_gauss() * noise_init;
                seed_y += state.rng.next_gauss() * noise_init;
            }
            // libmypaint's noise block arms the skip window before the
            // reset branch runs, but `brush_reset` (mypaint-brush.c:147)
            // clears `self->skip*` again — so a fresh stroke always starts
            // with the window DISARMED. Mirror that.
            state.skip_distance = 0.0;
            state.skip_last_x = 0.0;
            state.skip_last_y = 0.0;
            state.skipped_dtime = 0.0;
            // libmypaint then sets `self->random_input = rng_double_next()`
            // inside the reset branch — match that so the first real event
            // sees the same `INPUT(RANDOM)` value.
            state.random_input = state.rng.next_unit();
            // libmypaint stores the post-noise x/y as STATE.X/Y after the
            // reset (mypaint-brush.c:1404-1405). Use seed_x/seed_y so the
            // next event's interpolation starts from the noisy seed.
            state.last_event_x = seed_x;
            state.last_event_y = seed_y;
            state.last_event_time += dtime;
            state.actual_x = seed_x;
            state.actual_y = seed_y;
            state.actual_dab_x = seed_x;
            state.actual_dab_y = seed_y;
            state.last_dab_x = seed_x;
            state.last_dab_y = seed_y;
            state.dist_past_dab = 0.0;
            // Seed dynamic color from the brush's base color so per-dab drift
            // has somewhere to start.
            state.actual_h = self.get(BrushSetting::ColorH).base_value;
            state.actual_s = self.get(BrushSetting::ColorS).base_value;
            state.actual_v = self.get(BrushSetting::ColorV).base_value;
            state.smudge_buckets.fill(SmudgeBucket::default());
            state.reset_epoch = state.reset_epoch.wrapping_add(1);
            state.smudge_sample_dt = 0.0;
            state.smudge_sample_valid = false;
            state.smudge_sample_color = [0.0; 4];
            state.smudge_dabs_since_sample = 0;
            state.min_smudge_bucket_used = -1;
            state.max_smudge_bucket_used = -1;
            state.smudge_state = [0.0; 4];
            state.last_getcolor = [0.0; 4];
            state.last_getcolor_recentness = 0.0;
            state.attack_angle = 0.0;
            state.gridmap_x = 0.0;
            state.gridmap_y = 0.0;
            state.pressure = pressure;
            state.barrel_rotation = 0.0;
            state.split_due = false;
            state.norm_speed1_slow = 0.0;
            state.norm_speed2_slow = 0.0;
            state.norm_dx_slow = 0.0;
            state.norm_dy_slow = 0.0;
            state.direction_dx = 0.0;
            state.direction_dy = 0.0;
            state.direction_angle_dx = 0.0;
            state.direction_angle_dy = 0.0;
            state.stroke_total_painting_time = 0.0;
            state.stroke_current_idling_time = 0.0;
            // libmypaint's reset branch explicitly overrides the memset with
            // `STATE(STROKE) = 1.0` — "start in a state as if the stroke was
            // long finished" (mypaint-brush.c:1413), so stroke-curve brushes
            // begin at the held end value, not a fresh ramp.
            state.stroke_state = 1.0;
            state.stroke_started = false;
            state.custom_input = 0.0;
            state.flip = -1.0;
            // Part of libmypaint's `states[]` memset. `prev_settings`
            // (mirroring `settings_value[]`) deliberately survives the
            // reset, like upstream.
            state.actual_radius = 0.0;
            state.dabs_per_actual_radius = 0.0;
            state.dabs_per_basic_radius = 0.0;
            state.dabs_per_second = 0.0;
            state.actual_elliptical_ratio = 0.0;
            state.actual_elliptical_angle = 0.0;
            // Seed the smoothed tilt state at the event's input so the
            // first dab doesn't lerp away from a stale value.
            let m = (xtilt * xtilt + ytilt * ytilt).sqrt().min(1.0);
            // libmypaint's brush_reset (mypaint-brush.c:159) zeroes the
            // entire STATE struct via memset. tilt_declination /
            // tilt_ascension only ramp toward their no-tilt defaults
            // (90 / 0) once the per-dab step deltas in
            // update_states_and_setting_values run. hokusai used to
            // seed declination at 90 here, so curves keyed on
            // tilt_declination saw the saturated 90° value from the
            // first dab onwards instead of libmypaint's ramp.
            state.ascension = 0.0;
            state.declination = 0.0;
            state.declination_x = 0.0;
            state.declination_y = 0.0;
            // Silence unused warnings — xtilt/ytilt/m are still in scope
            // for later seed steps (state.X / state.Y).
            let _ = (xtilt, ytilt, m);
            state.started = true;
            // libmypaint's reset branch returns TRUE — "finished or empty" —
            // so the caller splits the current brushwork. Surface the same
            // through split_due (pigment's Brushwork consumes it).
            state.split_due = true;
            return false;
        }

        // --- Stroke input gating threshold ----------------------------------
        // libmypaint flips `STATE.STROKE_STARTED` based on pressure crossing
        // `stroke_threshold` (and `stroke_threshold * 0.9 + ε` on the way
        // down) — evaluated PER SIMULATION STEP on the interpolated pressure
        // (update_states_and_setting_values, mypaint-brush.c:665-676), not
        // once per event. The gate itself lives in the step loop below.
        let stroke_threshold = self.get(BrushSetting::StrokeThreshold).base_value.max(0.0);
        const STROKE_EPS: f32 = 0.0001;

        // --- Tilt-derived inputs --------------------------------------------
        // libmypaint convention (mypaint-brush.c):
        //   declination = 90° when the pen is straight up, decreasing as the
        //     pen tilts toward the tablet. Formula: `90 - hypot(xtilt,ytilt) * 60`.
        //   ascension   = `atan2(-xtilt, ytilt)` in degrees.
        // When no tilt is reported, libmypaint leaves declination at 90 and
        // ascension at 0 — anything else makes pressure-only strokes evaluate
        // brushes (e.g. marker_fat) as if the pen were lying flat against the
        // tablet, which feeds wildly wrong radius/aspect values into the
        // curves.
        let (tilt_mag, tilt_declination, tilt_ascension, tilt_declination_x, tilt_declination_y) =
            if xtilt == 0.0 && ytilt == 0.0 {
                // libmypaint initialises both axis targets to 90 when the
                // tablet reports no tilt (mypaint-brush.c:1306-1320).
                (0.0, 90.0, 0.0, 90.0, 90.0)
            } else {
                let m = (xtilt * xtilt + ytilt * ytilt).sqrt().min(1.0);
                (
                    m,
                    90.0 - m * 60.0,
                    (-xtilt).atan2(ytilt).to_degrees(),
                    xtilt * 60.0,
                    ytilt * 60.0,
                )
            };

        // --- Constant per-dab inputs -----------------------------------------
        // Inputs that don't vary across the simulation steps of this event.
        // Everything else (pressure, speeds, stroke, direction, tilt states,
        // random, custom, gridmap) is overridden per step inside the loop,
        // exactly like libmypaint recomputes `inputs[]` per
        // `update_states_and_setting_values` call.
        let mut dab_inputs = InputValues::new();
        dab_inputs.set(BrushInput::Tilt, tilt_mag);
        // libmypaint feeds `BASEVAL(RADIUS_LOGARITHMIC)` directly (`ln(r)`).
        dab_inputs.set(
            BrushInput::BrushRadius,
            self.get(BrushSetting::Radius).base_value,
        );

        // libmypaint applies `INPUT(PRESSURE) = STATE(PRESSURE) *
        // expf(BASEVAL(PRESSURE_GAIN_LOG))` (mypaint-brush.c:688).
        let pressure_gain = self.get(BrushSetting::PressureGainLog).base_value.exp();

        // libmypaint's *base_radius* is `expf(BASEVAL(RADIUS_LOGARITHMIC))` —
        // a brush-level constant unaffected by per-event input curves.
        // Several downstream calculations (offset_by_random jitter, the
        // dabs_per_basic_radius term, tracking_noise) scale by it rather
        // than the current dab radius.
        let base_radius = self
            .get(BrushSetting::Radius)
            .base_value
            .exp();

        // --- Slow tracking: advance smoothed position toward the event ------
        // libmypaint applies an exponential moving average with time
        // constant `0.01 * slow_tracking` seconds (the `0.01` makes the
        // setting's "displayed range" of 0–10 cover ~0–100 ms of lag).
        // Formula: approach = 1 - exp(-dt / (0.01 * slow)).
        // Read as BASEVAL — libmypaint ignores input curves here
        // (mypaint-brush.c:1391).
        let slow = self.get(BrushSetting::SlowTracking).base_value.max(0.0);
        let approach = if slow > 1e-3 {
            // C: `1 - exp_decay(BASEVAL(SLOW_TRACKING), 100.0 * dtime)`
            // — keep the same operation order for f32 bit-parity.
            1.0 - (-((100.0 * dtime) as f32) / slow).exp()
        } else {
            1.0
        };
        // --- Tracking noise: gaussian jitter on the raw input position ------
        // libmypaint adds the noise *before* slow_tracking smoothing, scaled
        // by `base_radius * BASEVAL(TRACKING_NOISE)`. The matching skip
        // mechanism (handled at the top of this function) coalesces
        // fast-arriving events so the noise sample rate tracks cursor
        // *distance*, not input frequency.
        let (mut noisy_x, mut noisy_y) = (x, y);
        let noise_mag = base_radius * self.get(BrushSetting::TrackingNoise).base_value.max(0.0);
        if noise_mag > 0.001 {
            // Arm the next skip window so subsequent events that arrive
            // before the cursor has travelled `0.5 * noise` pixels get
            // coalesced. Setting the bookkeeping fields before the RNG
            // calls matches libmypaint's order in `mypaint-brush.c`.
            state.skip_distance = 0.5 * noise_mag;
            state.skip_last_x = x;
            state.skip_last_y = y;
            noisy_x += state.rng.next_gauss() * noise_mag;
            noisy_y += state.rng.next_gauss() * noise_mag;
        }

        let prev_actual_x = state.actual_x;
        let prev_actual_y = state.actual_y;
        let new_actual_x = prev_actual_x + (noisy_x - prev_actual_x) * approach;
        let new_actual_y = prev_actual_y + (noisy_y - prev_actual_y) * approach;

        // libmypaint reads SMUDGE / SMUDGE_LENGTH per-dab; only SMUDGE_RADIUS_LOG
        // is treated as constant by `update_smudge_color`. Match that — the
        // per-dab versions are pulled from `dab_sv` inside the loop.
        // smudge_radius_log is now read per-dab from dab_sv inside the loop
        // — libmypaint reads SETTING(SMUDGE_RADIUS_LOG) inside
        // update_smudge_color (mypaint-brush.c line 854).
        // libmypaint's gate for entering `update_smudge_color` is
        // `SMUDGE != 0 || mapping not constant`. We need to know if the
        // mapping has any inputs so a curve-driven smudge that momentarily
        // evaluates to 0 still gets its recentness counter decayed.
        let smudge_setting = self.get(BrushSetting::Smudge);
        let smudge_mapping_active =
            smudge_setting.base_value != 0.0 || !mapping_is_constant(smudge_setting);
        // libmypaint selects the legacy smudge path when the *paint* setting
        // is at or below zero AND has a constant mapping
        // (`paint_factor <= 0.0 && paint_setting_constant`,
        // mypaint-brush.c:1111-1113). A curve that merely evaluates to zero
        // still uses the spectral path (get_color_pigment / mix_colors).
        let paint_setting_constant = mapping_is_constant(self.get(BrushSetting::Paint));
        // libmypaint's `if (offset_by_random)` consumes 2 PRNG draws on
        // ANY non-zero setting (curve-evaluated), then clamps the
        // amplitude with `MAX(0, ...)`. Hokusai used to gate the whole
        // block on `> 0`, so brushes whose pressure / custom curve pulled
        // the value below zero on some dabs (Posterizer, several scatter
        // brushes) silently lost two `next_gauss` consumptions vs the
        // libmypaint reference and the RNG sequences then diverged.
        // off_random / off_speed are now read per-dab from dab_sv (libmypaint
        // reads SETTING(...) per-dab in prepare_and_draw_dab). The block below
        // that gated RNG consumption on off_random_raw moves into the dab
        // loop too.

        // Running state for the inner loop. `cur_*` advances toward the
        // event's smoothed target one step at a time; libmypaint commits
        // these back into STATE after the final no-draw step below.
        //
        // libmypaint distinguishes STATE.X (smoothed cursor, advances toward
        // the slow-tracked target) from STATE.ACTUAL_X (the dab centre,
        // additionally lagged behind STATE.X by `slow_tracking_per_dab`).
        // `cur_x/cur_y` mirror STATE.X and `cur_ax/cur_ay` mirror
        // STATE.ACTUAL_X — `cur_ax/cur_ay` is where each dab actually lands.
        let mut cur_x = prev_actual_x;
        let mut cur_y = prev_actual_y;
        let mut cur_ax = state.actual_dab_x;
        let mut cur_ay = state.actual_dab_y;
        let mut cur_pressure = entry_pressure;
        // libmypaint keeps `dtime_left` as a DOUBLE and only narrows the
        // per-step slice (`step_dtime = frac * dtime_left`) to float —
        // mirror that so the time ledger rounds identically.
        let mut dtime_left: f64 = dtime;
        let mut dabs_moved = state.dist_past_dab;
        let target_x = new_actual_x;
        let target_y = new_actual_y;
        // SLOW_TRACKING_PER_DAB / SPEED*_SLOWNESS / OFFSET_BY_SPEED_SLOWNESS /
        // DIRECTION_FILTER are read per-dab from dab_sv after evaluation.

        // First dab count of the event: libmypaint's count_dabs_to reads
        // STATE.ACTUAL_RADIUS / .ACTUAL_ELLIPTICAL_* / .DABS_PER_* — all
        // carried over from the previous event's last simulation step (or
        // zeroed by a reset, triggering the base-value fallbacks).
        let mut dabs_todo = count_dabs_to(
            self,
            state,
            cur_x,
            cur_y,
            target_x,
            target_y,
            dtime_left as f32,
        );
        // libmypaint's `painted` is tri-state (UNKNOWN / YES / NO); the
        // distinction matters for stroke splitting (mypaint-brush.c:1420).
        let mut painted_state = 0u8;
        #[allow(unused_assignments)]
        let mut last_step_dpressure: Option<f32> = None;

        // Simulation-step loop: while a full dab is due, advance + evaluate +
        // draw. The final iteration (drawing == false) is libmypaint's
        // trailing "move the brush to the current time" call — the same state
        // advance and setting evaluation, but no dab and no RNG consumption.
        // The first drawing iteration only consumes `1 - dabs_moved` of a dab
        // so the accumulator picks up wherever the previous event left off.
        loop {
            let drawing = dabs_moved + dabs_todo >= 1.0;
            let (step_ddab, frac) = if drawing {
                let step_ddab = if dabs_moved > 0.0 {
                    1.0 - dabs_moved
                } else {
                    1.0
                };
                dabs_moved = 0.0;
                // No clamp — the loop condition guarantees
                // `step_ddab = 1 - dabs_moved <= dabs_todo`, like libmypaint's
                // bare `frac = step_ddab / dabs_todo`.
                (step_ddab, step_ddab / dabs_todo)
            } else {
                (dabs_todo, 1.0)
            };

            let step_dx = frac * (target_x - cur_x);
            let step_dy = frac * (target_y - cur_y);
            if trace_stepdx() {
                eprintln!(
                    "SDX dx={:08x} dy={:08x} frac={:08x} todo={:08x} tx={:08x} cx={:08x}",
                    step_dx.to_bits(),
                    step_dy.to_bits(),
                    frac.to_bits(),
                    dabs_todo.to_bits(),
                    target_x.to_bits(),
                    cur_x.to_bits(),
                );
            }
            let step_dpressure = frac * (pressure - cur_pressure);
            let step_dtime = (frac as f64 * dtime_left) as f32;
            last_step_dpressure = Some(step_dpressure);
            // libmypaint clamps degenerate step times at the top of
            // update_states_and_setting_values (mypaint-brush.c:616-622);
            // the *unclamped* value still drives the `dtime_left` ledger.
            let step_dtime_c = if step_dtime <= 0.0 { 0.001 } else { step_dtime };

            cur_x += step_dx;
            cur_y += step_dy;
            // libmypaint: `if (STATE(PRESSURE) <= 0.0) STATE(PRESSURE) = 0.0`.
            cur_pressure = (cur_pressure + step_dpressure).max(0.0);

            // Stroke start/end gate — per step, on the interpolated pressure.
            if !state.stroke_started && cur_pressure > stroke_threshold + STROKE_EPS {
                state.stroke_started = true;
                state.stroke_state = 0.0;
            } else if state.stroke_started && cur_pressure <= stroke_threshold * 0.9 + STROKE_EPS {
                state.stroke_started = false;
            }

            // Per-step velocity. libmypaint scales the world-space step by
            // `STATE(VIEWZOOM)` before filtering (mypaint-brush.c:772-773).
            let norm_dx = step_dx / step_dtime_c * state.viewzoom;
            let norm_dy = step_dy / step_dtime_c * state.viewzoom;
            let norm_speed = norm_dx.hypot(norm_dy);
            let norm_dist = {
                let ndx = step_dx / step_dtime_c / base_radius;
                let ndy = step_dy / step_dtime_c / base_radius;
                ndx.hypot(ndy) * step_dtime_c
            };

            // Advance tilt state toward the event's target. libmypaint
            // uses `frac * smallest_angular_difference(STATE.ASCENSION,
            // tilt_ascension)` for the ascension delta so a 359° → 1°
            // event lags by ~2°, not ~358°. Declination is a plain
            // additive delta. With `xtilt = ytilt = 0` the targets sit
            // at the libmypaint defaults (ascension 0, declination 90).
            let step_ascension = frac * smallest_angular_diff(state.ascension, tilt_ascension);
            let step_declination = frac * (tilt_declination - state.declination);
            let step_decl_x = frac * (tilt_declination_x - state.declination_x);
            let step_decl_y = frac * (tilt_declination_y - state.declination_y);
            state.ascension += step_ascension;
            state.declination += step_declination;
            state.declination_x += step_decl_x;
            state.declination_y += step_decl_y;
            let step_barrel_rotation =
                frac * smallest_angular_diff(state.barrel_rotation, barrel_rotation * 360.0);
            state.barrel_rotation += step_barrel_rotation;
            // libmypaint's update_states_and_setting_values evaluates ALL
            // SETTINGS using PRE-update STATE values (line 728), then
            // updates STATE (slow_tracking_per_dab / norm_speed_slow /
            // norm_dx/dy_slow / direction_dx/dy) using the freshly
            // evaluated SETTING smoothing factors. Tilt is the exception
            // — it's a raw event delta and stays before evaluation.
            // The state-update block moves to AFTER dab_sv = evaluate so
            // INPUT(DIRECTION) / INPUT(SPEED*) sample the lagged values.

            // libmypaint's update_states_and_setting_values evaluates ALL
            // settings using the PRE-advance STROKE, and only advances
            // STATE.STROKE afterwards using the freshly-evaluated
            // SETTING(STROKE_DURATION_LOGARITHMIC). Mirror that: feed the
            // current state.stroke_state into dab_inputs, evaluate dab_sv,
            // then advance below using dab_sv's per-dab values.
            dab_inputs.set(BrushInput::Pressure, cur_pressure * pressure_gain);
            dab_inputs.set(BrushInput::Stroke, state.stroke_state.min(1.0));
            // AttackAngle is event-level (depends on raw direction, not the
            // per-dab interpolated state) so inheriting from `inputs` is
            // already correct — no per-dab override needed.
            dab_inputs.set(
                BrushInput::Speed1,
                speed_input(
                    state.norm_speed1_slow,
                    self.get(BrushSetting::Speed1Gamma).base_value,
                ),
            );
            dab_inputs.set(
                BrushInput::Speed2,
                speed_input(
                    state.norm_speed2_slow,
                    self.get(BrushSetting::Speed2Gamma).base_value,
                ),
            );
            dab_inputs.set(BrushInput::Random, state.random_input);

            // Smoothed tilt inputs: libmypaint feeds the lagged STATE
            // values into INPUT(TILT_*) and INPUT(ATTACK_ANGLE) at the
            // per-dab evaluation, with the view rotation folded in.
            let asc_wrapped =
                mod_arith_c(state.ascension + state.viewrotation + 180.0, 360.0) - 180.0;
            dab_inputs.set(BrushInput::TiltDeclination, state.declination);
            dab_inputs.set(BrushInput::TiltAscension, asc_wrapped);
            dab_inputs.set(BrushInput::TiltDeclinationX, state.declination_x);
            dab_inputs.set(BrushInput::TiltDeclinationY, state.declination_y);
            dab_inputs.set(
                BrushInput::Viewzoom,
                self.get(BrushSetting::Radius).base_value - (base_radius / state.viewzoom).ln(),
            );
            dab_inputs.set(
                BrushInput::BarrelRotation,
                mod_arith_c(state.barrel_rotation, 360.0),
            );
            // libmypaint uses the SMOOTHED direction state (DIRECTION_DX/DY,
            // dir_angle_360 = atan2(DIRECTION_DY, DIRECTION_DX)) for the
            // attack_angle input — see mypaint-brush.c:712. Hokusai
            // previously used raw event direction here, which diverges
            // when direction_filter is non-zero (Posterizer drives
            // offset_angle_2 and offset_angle_adj via this input).
            let attack = attack_angle(
                state.ascension,
                state.direction_angle_dx,
                state.direction_angle_dy,
            );
            dab_inputs.set(BrushInput::AttackAngle, attack);

            // Custom input: feed the previous-dab smoothed value so the
            // curve in `evaluate` below can reference it (libmypaint pushes
            // the *prior* STATE.CUSTOM_INPUT into INPUT(CUSTOM) and only
            // refreshes the state after the dab is drawn — see the
            // `STATE.CUSTOM_INPUT += ...` block right below).
            dab_inputs.set(BrushInput::Custom, state.custom_input);
            // Smoothed direction inputs (per libmypaint's DIRECTION_DX/DY
            // and DIRECTION_ANGLE_DX/DY) — replace the event-level raw
            // direction we seeded `inputs` with.
            dab_inputs.set(
                BrushInput::Direction,
                direction_input(state.direction_dx, state.direction_dy, state.viewrotation),
            );
            dab_inputs.set(
                BrushInput::DirectionAngle,
                direction_angle(
                    state.direction_angle_dx,
                    state.direction_angle_dy,
                    state.viewrotation,
                ),
            );

            // libmypaint computes GRIDMAP_X / GRIDMAP_Y from the (lagged)
            // dab centre, scaled by `exp(SETTING(GRIDMAP_SCALE))` and the
            // per-axis SETTING multipliers (mypaint-brush.c:644-646). The
            // gridmap state update runs BEFORE this step's evaluation, so
            // SETTING() holds the previous step's values — read them from
            // the carried `prev_settings`.
            const GRID_SIZE: f32 = 256.0;
            let gscale = state
                .prev_settings
                .get(BrushSetting::GridmapScale)
                .exp();
            let gscale_x = state.prev_settings.get(BrushSetting::GridmapScaleX);
            let gscale_y = state.prev_settings.get(BrushSetting::GridmapScaleY);
            let scaled_size = gscale * GRID_SIZE;
            // C uses `mod_arith(fabsf(x * scale), scaled_size)` — the
            // subtraction form, not f32::rem_euclid's fmod form.
            let mut gx =
                mod_arith_c((cur_ax * gscale_x).abs(), scaled_size) / scaled_size * GRID_SIZE;
            let mut gy =
                mod_arith_c((cur_ay * gscale_y).abs(), scaled_size) / scaled_size * GRID_SIZE;
            if cur_ax < 0.0 {
                gx = GRID_SIZE - gx;
            }
            if cur_ay < 0.0 {
                gy = GRID_SIZE - gy;
            }
            state.gridmap_x = gx;
            state.gridmap_y = gy;
            dab_inputs.set(BrushInput::GridmapX, gx.clamp(0.0, GRID_SIZE));
            dab_inputs.set(BrushInput::GridmapY, gy.clamp(0.0, GRID_SIZE));

            if trace_inputbits() {
                eprintln!(
                    "IN p={:08x} r={:08x} st={:08x} dir={:08x} s1={:08x} s2={:08x} ns2={:08x} sst={:08x} cu={:08x} ddx={:08x} ddy={:08x}",
                    dab_inputs.get(BrushInput::Pressure).to_bits(),
                    dab_inputs.get(BrushInput::Random).to_bits(),
                    dab_inputs.get(BrushInput::Stroke).to_bits(),
                    dab_inputs.get(BrushInput::Direction).to_bits(),
                    dab_inputs.get(BrushInput::Speed1).to_bits(),
                    dab_inputs.get(BrushInput::Speed2).to_bits(),
                    state.norm_speed2_slow.to_bits(),
                    state.stroke_state.to_bits(),
                    dab_inputs.get(BrushInput::Custom).to_bits(),
                    state.direction_dx.to_bits(),
                    state.direction_dy.to_bits(),
                );
            }
            let dab_sv = evaluate(self, &dab_inputs);
            // libmypaint evaluates the current settings first, then commits
            // these STATE values (mypaint-brush.c:818-824). count_dabs_to()
            // and opaque_linearize read them for the next dab.
            state.dabs_per_actual_radius = dab_sv.get(BrushSetting::DabsPerActualRadius);
            state.dabs_per_basic_radius = dab_sv.get(BrushSetting::DabsPerBasicRadius);
            state.dabs_per_second = dab_sv.get(BrushSetting::DabsPerSecond);
            // Mirrors libmypaint's `print_inputs` debug hook (enable both
            // sides with HOKUSAI_TRACE_INPUTS=1 and paste-diff the streams).
            if trace_inputs() {
                eprintln!(
                    "press={:6.3}, speed1={:7.4}\tspeed2={:7.4}\tstroke={:6.3}\tcustom={:6.3}\tdir={:6.3}\tdec={:6.3}\tasc={:6.3}\trand={:6.3}\tattack={:6.3}",
                    dab_inputs.get(BrushInput::Pressure),
                    dab_inputs.get(BrushInput::Speed1),
                    dab_inputs.get(BrushInput::Speed2),
                    dab_inputs.get(BrushInput::Stroke),
                    dab_inputs.get(BrushInput::Custom),
                    dab_inputs.get(BrushInput::Direction),
                    dab_inputs.get(BrushInput::TiltDeclination),
                    dab_inputs.get(BrushInput::TiltAscension),
                    dab_inputs.get(BrushInput::Random),
                    dab_inputs.get(BrushInput::AttackAngle),
                );
            }
            let dab_radius = dab_sv.get(BrushSetting::Radius).exp().clamp(0.2, 1000.0);
            state.actual_radius = dab_radius;

            // ===== Post-evaluate STATE updates (libmypaint:732-797) =====
            let slow_per_dab_d = dab_sv.get(BrushSetting::SlowTrackingPerDab).max(0.0);
            let fac_ax = if slow_per_dab_d > 1e-3 {
                1.0 - (-step_ddab / slow_per_dab_d).exp()
            } else {
                1.0
            };
            cur_ax += (cur_x - cur_ax) * fac_ax;
            cur_ay += (cur_y - cur_ay) * fac_ax;

            let slow1_d = dab_sv.get(BrushSetting::Speed1Slowness).max(0.0);
            let slow2_d = dab_sv.get(BrushSetting::Speed2Slowness).max(0.0);
            let fac1 = if slow1_d > 1e-3 {
                1.0 - (-step_dtime_c / slow1_d).exp()
            } else {
                1.0
            };
            let fac2 = if slow2_d > 1e-3 {
                1.0 - (-step_dtime_c / slow2_d).exp()
            } else {
                1.0
            };
            state.norm_speed1_slow += (norm_speed - state.norm_speed1_slow) * fac1;
            state.norm_speed2_slow += (norm_speed - state.norm_speed2_slow) * fac2;

            let speed_off_slow_d = dab_sv.get(BrushSetting::OffsetBySpeedSlowness);
            let speed_off_tc = ((speed_off_slow_d * 0.01).exp() - 1.0).max(0.002);
            let fac_dx = 1.0 - (-step_dtime_c / speed_off_tc).exp();
            state.norm_dx_slow += (norm_dx - state.norm_dx_slow) * fac_dx;
            state.norm_dy_slow += (norm_dy - state.norm_dy_slow) * fac_dx;

            let dir_filter_d = dab_sv.get(BrushSetting::DirectionFilter).max(0.0);
            let dir_time_const = (dir_filter_d * 0.5).exp() - 1.0;
            // libmypaint filters the viewzoom-scaled step vector here
            // (`dx = step_dx * STATE.VIEWZOOM`, mypaint-brush.c:851-856).
            // The uniform scale cancels in the atan2 inputs, but it changes
            // the low-pass time step (`step_in_dabtime`) and therefore the
            // filter strength whenever the canvas is zoomed.
            let dir_dx = step_dx * state.viewzoom;
            let dir_dy = step_dy * state.viewzoom;
            let step_in_dabtime = dir_dx.hypot(dir_dy);
            let dir_fac = if dir_time_const > 1e-3 {
                1.0 - (-step_in_dabtime / dir_time_const).exp()
            } else {
                1.0
            };
            if trace_dir() {
                eprintln!(
                    "DIRF dx={:08x} dy={:08x} t={:08x} fac={:08x} tc={:08x} sdx={:08x} sdy={:08x} cx={:08x}",
                    dir_dx.to_bits(),
                    dir_dy.to_bits(),
                    step_in_dabtime.to_bits(),
                    dir_fac.to_bits(),
                    dir_time_const.to_bits(),
                    step_dx.to_bits(),
                    step_dy.to_bits(),
                    state.actual_x.to_bits(),
                );
            }
            state.direction_angle_dx += (dir_dx - state.direction_angle_dx) * dir_fac;
            state.direction_angle_dy += (dir_dy - state.direction_angle_dy) * dir_fac;
            let (mut dx_for_dir, mut dy_for_dir) = (dir_dx, dir_dy);
            let dx_old = state.direction_dx;
            let dy_old = state.direction_dy;
            let pos_dist = (dx_old - dx_for_dir).powi(2) + (dy_old - dy_for_dir).powi(2);
            let neg_dist = (dx_old + dx_for_dir).powi(2) + (dy_old + dy_for_dir).powi(2);
            if pos_dist > neg_dist {
                dx_for_dir = -dx_for_dir;
                dy_for_dir = -dy_for_dir;
            }
            state.direction_dx += (dx_for_dir - state.direction_dx) * dir_fac;
            state.direction_dy += (dy_for_dir - state.direction_dy) * dir_fac;

            // Advance STATE.STROKE by this step's normalised distance,
            // using the per-dab evaluated stroke_duration / stroke_holdtime
            // (libmypaint reads them as SETTING, not BASEVAL). The wrap rule
            // saturates at 1.0 when stroke_holdtime >= ~9.9 (a hold-forever
            // signal), otherwise modulos `1 + stroke_holdtime` so periodic
            // stroke-driven curves cycle.
            {
                let stroke_freq = (-dab_sv.get(BrushSetting::StrokeDurationLogarithmic)).exp();
                let stroke_wrap = 1.0 + dab_sv.get(BrushSetting::StrokeHoldtime).max(0.0);
                let mut stroke_advance = (state.stroke_state + norm_dist * stroke_freq).max(0.0);
                if stroke_advance >= stroke_wrap {
                    if stroke_wrap > 10.9 {
                        stroke_advance = 1.0;
                    } else {
                        stroke_advance %= stroke_wrap;
                    }
                }
                state.stroke_state = stroke_advance;
            }

            // Refresh STATE.CUSTOM_INPUT toward the freshly evaluated
            // SETTING(custom_input). libmypaint uses a fixed `0.1`
            // pseudo-`dt` here (the slowness is measured in "10× longer is
            // 10× slower"), so the smoothing strength doesn't depend on
            // the per-dab step time.
            let cust_slow = dab_sv.get(BrushSetting::CustomInputSlowness).max(0.0);
            let cust_fac = if cust_slow > 1e-3 {
                1.0 - (-0.1 / cust_slow).exp()
            } else {
                1.0
            };
            let cust_target = dab_sv.get(BrushSetting::CustomInput);
            state.custom_input += (cust_target - state.custom_input) * cust_fac;

            // STATE.ACTUAL_ELLIPTICAL_DAB_* ← fresh SETTING values
            // (mypaint-brush.c:807-809). The angle is folded into
            // `[-180, 0)` and corrected for the view rotation.
            state.actual_elliptical_ratio = dab_sv.get(BrushSetting::EllipticalDabRatio);
            // C: `mod_arith(SETTING(ANGLE) - viewrotation + 180.0, 180.0)
            // - 180.0`. The `+ 180.0` / `- 180.0` literals are double, so
            // both adds round once when narrowed back to f32.
            let angle_plus = ((dab_sv.get(BrushSetting::EllipticalDabAngle)
                - state.viewrotation) as f64
                + 180.0) as f32;
            state.actual_elliptical_angle =
                (mod_arith_c(angle_plus, 180.0) as f64 - 180.0) as f32;

            // Mirror of libmypaint's `settings_value[]`: keep the full
            // evaluation for the next step's delayed reads (DABS_PER_*,
            // gridmap scales).
            state.prev_settings = dab_sv.clone();

            // Final no-draw step ends here — libmypaint's trailing
            // update_states_and_setting_values call advances the state but
            // draws nothing and consumes no RNG.
            if !drawing {
                break;
            }

            let mut px = cur_ax;
            let mut py = cur_ay;

            // Toggle libmypaint's `STATE.FLIP` so `offset_angle_2*` can
            // mirror dabs across the stroke direction. Done *before* the
            // offsets so this dab gets the freshly toggled sign.
            state.flip = -state.flip;
            let (off_x, off_y) = directional_offsets(
                &dab_sv,
                base_radius,
                state.flip,
                state.direction_angle_dx,
                state.direction_angle_dy,
                state.ascension,
                state.viewrotation,
            );
            px += off_x;
            py += off_y;
            if trace_pos() {
                eprintln!(
                    "POS ax={:08x} ay={:08x} dox={:08x} doy={:08x} ndx={:08x} ndy={:08x} br={:08x} vz={:08x}",
                    cur_ax.to_bits(), cur_ay.to_bits(), off_x.to_bits(), off_y.to_bits(),
                    state.norm_dx_slow.to_bits(), state.norm_dy_slow.to_bits(),
                    base_radius.to_bits(), state.viewzoom.to_bits(),
                );
            }

            // libmypaint applies `offset_by_speed` (mypaint-brush.c:1086)
            // before the random offset (1092). The addition order is
            // observable in f32, so keep it.
            let off_speed = dab_sv.get(BrushSetting::OffsetBySpeed);
            if off_speed != 0.0 {
                // C evaluates `norm * offset` in float, then `* 0.1` and
                // `/ viewzoom` in double (0.1 is a double literal) and adds
                // that to the float cursor in double — round once.
                let tdx = state.norm_dx_slow * off_speed;
                let tdy = state.norm_dy_slow * off_speed;
                let off_x = tdx as f64 * 0.1 / state.viewzoom as f64;
                let off_y = tdy as f64 * 0.1 / state.viewzoom as f64;
                px = (px as f64 + off_x) as f32;
                py = (py as f64 + off_y) as f32;
            }
            if trace_pos() {
                eprintln!("POSP x={:08x} y={:08x}", px.to_bits(), py.to_bits());
            }

            // Truthy check matches libmypaint — any non-zero curve value
            // burns 2 PRNG draws so the sequence stays in lock-step even
            // when the curve dips negative (where amplitude clamps to 0).
            let off_random_raw = dab_sv.get(BrushSetting::OffsetByRandom);
            if off_random_raw != 0.0 {
                let off_random = off_random_raw.max(0.0);
                px += state.rng.next_gauss() * off_random * base_radius;
                py += state.rng.next_gauss() * off_random * base_radius;
            }
            // libmypaint's `radius_by_random`: gaussian-jitter
            // `radius_logarithmic` by `noise * setting`, clamp the result
            // into `[ACTUAL_RADIUS_MIN, ACTUAL_RADIUS_MAX]`, and scale
            // opaque by `(orig_radius / new_radius)²` when the new radius
            // is bigger so the perceived ink density stays even. Consumes
            // one PRNG draw — placed *after* `offset_by_random` to keep
            // the consumption order aligned with `prepare_and_draw_dab`.
            // Same truthy gating as offset_by_random above.
            let mut dab_opaque_scale = 1.0_f32;
            // The perturbed radius is a LOCAL in libmypaint
            // (prepare_and_draw_dab's `float radius`) — it feeds the dab
            // geometry and the smudge sample radius, but STATE.ACTUAL_RADIUS
            // keeps the unperturbed `exp(SETTING(radius_log))`, so
            // count_dabs_to stays stable across noisy dabs. hokusai used to
            // write the noise back into the state, which made the dab
            // spacing itself jitter and threw every downstream
            // frac/step_dtime off the reference sequence.
            let mut draw_radius = dab_radius;
            let rad_random_raw = dab_sv.get(BrushSetting::RadiusByRandom);
            if rad_random_raw != 0.0 {
                // libmypaint applies the signed setting value
                // (`rand_gauss() * radius_by_random`, mypaint-brush.c:1100).
                // Curves such as Dieterle's `custom → ±0.07` alternate
                // between shrinking and growing the dab; clamping the
                // amplitude to zero removes half of that modulation while
                // still consuming the PRNG draw.
                let noise = state.rng.next_gauss() * rad_random_raw;
                let new_log = dab_sv.get(BrushSetting::Radius) + noise;
                draw_radius = new_log.exp().clamp(0.2, 1000.0);
                let alpha_correction = (dab_radius / draw_radius).powi(2);
                if alpha_correction <= 1.0 {
                    dab_opaque_scale = alpha_correction;
                }
            }
            // libmypaint's `update_smudge_color`: lazy canvas resample
            // (controlled by `smudge_length_log` / recentness counter),
            // then the legacy/spectral mix into the smudge bucket. The
            // sampled colour can also gate-out the whole dab via
            // `smudge_transparency`.
            // libmypaint evaluates SMUDGE and SMUDGE_LENGTH per-dab.
            let smudge_amt = dab_sv.get(BrushSetting::Smudge).clamp(0.0, 1.0);
            let smudge_length = dab_sv.get(BrushSetting::SmudgeLength).clamp(0.0, 1.0);
            let bucket_index = (dab_sv.get(BrushSetting::SmudgeBucket).round() as i32)
                .clamp(0, state.smudge_buckets.len() as i32 - 1)
                as usize;
            if state.min_smudge_bucket_used == -1
                || state.min_smudge_bucket_used > bucket_index as i32
            {
                state.min_smudge_bucket_used = bucket_index as i32;
            }
            if state.max_smudge_bucket_used < bucket_index as i32 {
                state.max_smudge_bucket_used = bucket_index as i32;
            }
            let mut bucket = state.smudge_buckets[bucket_index];
            let paint_factor = dab_sv.get(BrushSetting::Paint);
            let paint_mode = paint_factor.clamp(0.0, 1.0);
            let legacy_smudge = paint_factor <= 0.0 && paint_setting_constant;
            let smudge_length_log = dab_sv.get(BrushSetting::SmudgeLengthLog);
            let smudge_radius_log = dab_sv.get(BrushSetting::SmudgeRadiusLog);
            // libmypaint feeds the post-radius_by_random radius into
            // update_smudge_color (`radius` at mypaint-brush.c:1044,
            // reassigned in the radius_by_random branch a few lines earlier).
            let smudge_radius = (draw_radius * smudge_radius_log.exp()).clamp(0.2, 1000.0);
            // libmypaint passes the raw paint factor to get_color; the sign
            // selects legacy sampling inside the surface.
            let paint_for_sample = paint_factor;

            // GPU-deferred smudge. The device runs the sample, the bucket
            // update and the dab colour chain in submission order, so the
            // canvas already contains every dab queued before this one.
            //
            // Scope: the spectral paint path with `smudge_length_log == 0`
            // (always resample) and no HSV/HSL colour dynamics. Those are
            // exactly the brushes that currently pay one submit+fence per dab.
            let defer_brush = smudge_mapping_active
                && paint_mode == 1.0
                && smudge_length_log == 0.0
                && !color_dynamics_active(&dab_sv);
            let update_active = smudge_length < 1.0;
            let mut precomputed = None;
            if defer_brush
                && surface.defer_smudge()
                && update_active
                && would_resample(&dab_sv, &bucket, smudge_length, smudge_mapping_active)
            {
                let interval = smudge_sample_dab_interval();
                state.smudge_dabs_since_sample =
                    state.smudge_dabs_since_sample.saturating_add(1);
                let due = !state.smudge_sample_valid
                    || interval == 0
                    || state.smudge_dabs_since_sample >= interval;
                if due {
                    // libmypaint calls get_color with `ROUND(x), ROUND(y)`
                    // (mypaint-brush.c:1128); sample the integer pixel grid.
                    let sample_x = round_c_pixel(px);
                    let sample_y = round_c_pixel(py);
                    if let Some(c) = surface.get_color_pigment_gpu(
                        sample_x,
                        sample_y,
                        smudge_radius,
                        paint_for_sample,
                    ) {
                        state.smudge_sample_valid = true;
                        state.smudge_sample_color = [c.r, c.g, c.b, c.a];
                        state.smudge_dabs_since_sample = 0;
                    }
                }
                if state.smudge_sample_valid {
                    let c = state.smudge_sample_color;
                    precomputed = Some(RgbaF32 {
                        r: c[0],
                        g: c[1],
                        b: c[2],
                        a: c[3],
                    });
                }
            }
            let (new_bucket, skip_dab) = update_smudge_bucket_cpu(
                surface,
                &dab_sv,
                bucket,
                px,
                py,
                draw_radius,
                smudge_length,
                smudge_mapping_active,
                legacy_smudge,
                if defer_brush { precomputed } else { None },
            );
            bucket = new_bucket;
            state.smudge_buckets[bucket_index] = bucket;

            // libmypaint computes the dab colour entirely inside
            // `prepare_and_draw_dab`: brush base → apply_smudge → eraser
            // → HSV/HSL dynamics, in that order. We do the same in
            // `build_dab` now (it took `&mut state` for `state.actual_*`
            // bookkeeping) — no separate per-dab drift pass.
            let dab = build_dab(
                self,
                &dab_sv,
                state,
                px,
                py,
                draw_radius,
                smudge_amt,
                dab_opaque_scale,
                bucket,
                legacy_smudge,
                linear,
            );
            if trace_dabargs() {
                use std::sync::atomic::{AtomicU64, Ordering};
                static DAB_SEQ: AtomicU64 = AtomicU64::new(0);
                let dab_seq = DAB_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
                eprintln!(
                    "DAB n={} x={:08x} y={:08x} r={:08x} cr={:08x} cg={:08x} cb={:08x} op={:08x} ha={:08x} so={:08x} ae={:08x} as={:08x} an={:08x} la={:08x} co={:08x} po={:08x} pn={:08x} pa={:08x}",
                    dab_seq,
                    dab.x.to_bits(), dab.y.to_bits(), dab.radius.to_bits(),
                    dab.color.r.to_bits(), dab.color.g.to_bits(), dab.color.b.to_bits(),
                    dab.opaque.to_bits(), dab.hardness.to_bits(), dab.softness.to_bits(),
                    dab.alpha_eraser.to_bits(), dab.aspect_ratio.to_bits(), dab.angle.to_bits(),
                    dab.lock_alpha.to_bits(), dab.colorize.to_bits(), dab.posterize.to_bits(),
                    dab.posterize_num.to_bits(), dab.paint.to_bits(),
                );
            }
            if !skip_dab && surface.draw_dab(&dab) {
                painted_state = 1;
            } else if painted_state == 0 {
                painted_state = 2;
            }
            state.last_dab_x = dab.x;
            state.last_dab_y = dab.y;
            // libmypaint refreshes `random_input` once per dab, *after*
            // drawing.
            state.random_input = state.rng.next_unit();

            dtime_left -= step_dtime as f64;
            if trace_count() {
                eprintln!(
                    "STEP sd={:08x} left={:08x} dd={:08x} todo={:08x} dp={:08x} p={:08x}",
                    step_dtime.to_bits(),
                    (dtime_left as f32).to_bits(),
                    step_ddab.to_bits(),
                    dabs_todo.to_bits(),
                    step_dpressure.to_bits(),
                    cur_pressure.to_bits()
                );
            }
            // Recount against the freshly advanced state — STATE.ACTUAL_RADIUS
            // (possibly radius_by_random-perturbed), this dab's elliptical
            // aspect/angle, and the one-step-delayed DABS_PER_* values.
            dabs_todo = count_dabs_to(
                self,
                state,
                cur_x,
                cur_y,
                target_x,
                target_y,
                dtime_left as f32,
            );
        }

        // `dabs_moved` survives the final no-draw step untouched, so the
        // fractional leftover carries to the next event — libmypaint's
        // `STATE(PARTIAL_DABS) = dabs_moved + dabs_todo`.
        state.dist_past_dab = dabs_moved + dabs_todo;

        // --- Commit event state ---------------------------------------------
        state.last_event_x = x;
        state.last_event_y = y;
        state.last_event_time += dtime;
        state.actual_x = new_actual_x;
        state.actual_y = new_actual_y;
        state.actual_dab_x = cur_ax;
        state.actual_dab_y = cur_ay;
        state.pressure = cur_pressure;

        let painted = match painted_state {
            1 => true,
            2 => false,
            _ => {
                // Still UNKNOWN: libmypaint assumes painting if the stroke
                // has painted before and is not idling.
                !(state.stroke_current_idling_time > 0.0 || state.stroke_total_painting_time == 0.0)
            }
        };
        if painted {
            state.stroke_total_painting_time += dtime;
            state.stroke_current_idling_time = 0.0;
            // Force a stroke split after a while, but only while pressure is
            // not being released (mypaint-brush.c:1521-1527).
            if state.stroke_total_painting_time > 4.0 + 3.0 * pressure as f64
                && last_step_dpressure.unwrap_or(0.0) >= 0.0
            {
                state.split_due = true;
            }
        } else {
            state.stroke_current_idling_time += dtime;
            if state.stroke_total_painting_time == 0.0 {
                if state.stroke_current_idling_time > 1.0 {
                    state.split_due = true;
                }
            } else if state.stroke_total_painting_time + state.stroke_current_idling_time
                > 0.9 + 5.0 * pressure as f64
            {
                state.split_due = true;
            }
        }

        painted
    }

    /// Flush `slow_tracking` lag and paint the trailing pixels.
    ///
    /// Call this on pointer-up. The smoothed position lags behind the live
    /// cursor by up to `velocity * τ` pixels (where `τ ≈ 0.01 * slow_tracking`
    /// seconds). Without flushing, that trail is left unpainted — so a
    /// stroke ending at x=660 would only have paint up to x≈645 for a brush
    /// with `slow_tracking=3` at 500 px/s.
    ///
    /// Pumps a handful of small idle events at the last cursor position so
    /// the smoothed position catches up. Returns `true` if any pixel was
    /// painted.
    pub fn finish_stroke<S: TiledSurface>(&self, state: &mut BrushState, surface: &mut S) -> bool {
        self.finish_stroke_full(state, surface, false)
    }

    /// [`Brush::finish_stroke`] with an explicit colour-space flag, matching
    /// the `linear` argument of [`Brush::stroke_to_full`]. MyPaint's caller
    /// passes `eotf() != 1.0` (default `true`).
    pub fn finish_stroke_full<S: TiledSurface>(
        &self,
        state: &mut BrushState,
        surface: &mut S,
        linear: bool,
    ) -> bool {
        if !state.started {
            return false;
        }
        let mut painted = false;
        // Up to 8 × 16 ms ≈ 130 ms of catch-up. With τ ≤ 100 ms (slow ≤ 10)
        // that's ≥ 1 time constant, leaving < 37 % residual lag; for typical
        // brushes (slow ≤ 5) it's ≥ 2.5 τ and < 8 %. Pressure is held at the
        // last received value so brushes whose `opaque` is pressure-driven
        // keep painting along the trailing segment.
        let p = state.last_pressure;
        for _ in 0..8 {
            painted |= self.stroke_to_full(
                state,
                surface,
                state.last_event_x,
                state.last_event_y,
                p,
                0.0,
                0.0,
                0.016,
                state.viewzoom,
                state.viewrotation.to_radians(),
                state.barrel_rotation / 360.0,
                linear,
            );
            let lag = ((state.last_event_x - state.actual_x).powi(2)
                + (state.last_event_y - state.actual_y).powi(2))
            .sqrt();
            if lag < 0.5 {
                break;
            }
        }
        painted
    }
}

// Color dynamics are folded directly into `build_dab` so they apply
// *after* `apply_smudge` — matching libmypaint's order in
// `prepare_and_draw_dab`.

/// Returns the effective `opaque_multiply` factor.
///
/// libmypaint has no "unset means 1.0" special case: it multiplies by
/// `SETTING(OPAQUE_MULTIPLY)` directly (mypaint-brush.c:1044-1047), and an
/// explicit base value of 0 with no inputs zeroes every dab. The 1.0
/// identity lives in the *default* installed by
/// `mypaint_brush_from_defaults` (base 0 plus a `[(0,0),(1,1)]` pressure
/// curve) and in the `.myb` parser, not in the stroke engine.
fn opaque_multiplier(sv: &SettingValues) -> f32 {
    // libmypaint multiplies by the raw setting value and clamps the *product*
    // (`CLAMP(opaque * opaque_fac, 0, 1)`, mypaint-brush.c:1046-1047).
    // Pre-clamping the factor to 1 silently caps the 0..2 range the setting
    // allows (e.g. 0.5 opaque × 2 should reach full opacity).
    sv.get(BrushSetting::OpaqueMultiply)
}

/// Brush base colour in the engine's working space (RGB only).
///
/// Shared by [`build_dab`] and the deferred-smudge request so both sides mix
/// the same source colour into the bucket.
fn brush_base_linear(brush: &Brush, linear: bool) -> [f32; 3] {
    let h = brush.get(BrushSetting::ColorH).base_value;
    let s = brush.get(BrushSetting::ColorS).base_value;
    let v = brush.get(BrushSetting::ColorV).base_value;
    if !linear {
        let base = hsv_to_rgb(Hsv { h, s, v });
        return [base.r, base.g, base.b];
    }
    // MyPaint's Python wrapper runs helpers.transform_hsv in double:
    // HSV -> RGB, each channel ** 2.2, RGB -> HSV; the result is stored
    // as float and libmypaint converts HSV -> RGB again. Reproduce the
    // double intermediates before narrowing, then use the C float HSV
    // conversion for the final RGB.
    let (r, g, b) = hsv_to_rgb_f64(h as f64, s as f64, v as f64);
    let (r, g, b) = (r.powf(2.2), g.powf(2.2), b.powf(2.2));
    let (h, s, v) = rgb_to_hsv_f64(r, g, b);
    let rgb = hsv_to_rgb(Hsv {
        h: h as f32,
        s: s as f32,
        v: v as f32,
    });
    [rgb.r, rgb.g, rgb.b]
}

fn hsv_to_rgb_f64(h: f64, s: f64, v: f64) -> (f64, f64, f64) {
    if s == 0.0 {
        return (v, v, v);
    }
    let h = (h - h.floor()) * 6.0;
    let i = h.floor();
    let f = h - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match (i as i32).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

fn rgb_to_hsv_f64(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    if delta == 0.0 {
        return (0.0, 0.0, max);
    }
    let h = if r == max {
        (g - b) / delta
    } else if g == max {
        2.0 + (b - r) / delta
    } else {
        4.0 + (r - g) / delta
    };
    let mut h = h / 6.0;
    if h < 0.0 {
        h += 1.0;
    }
    (h, delta / max, max)
}

/// libmypaint's `mypaint_mapping_is_constant`: an input only counts once it
/// has mapping points (parse may leave empty mapping vectors behind).
#[inline]
fn mapping_is_constant(sv: &crate::mapping::SettingValue) -> bool {
    !sv.inputs.iter().any(|m| !m.points.is_empty())
}

/// HSV/HSL dynamics are not implemented in the GPU smudge chain yet.
#[inline]
fn color_dynamics_active(sv: &SettingValues) -> bool {
    sv.get(BrushSetting::ChangeColorH) != 0.0
        || sv.get(BrushSetting::ChangeColorV) != 0.0
        || sv.get(BrushSetting::ChangeColorHsvS) != 0.0
        || sv.get(BrushSetting::ChangeColorL) != 0.0
        || sv.get(BrushSetting::ChangeColorHslS) != 0.0
}

/// Number of dabs between device smudge samples. A time gate would let the
/// dab queue grow between samples and turn the next sample into a huge flush;
/// gating by dab count keeps both the sample rate and the flush size bounded.
fn smudge_sample_dab_interval() -> u32 {
    use std::sync::OnceLock;
    static DABS: OnceLock<u32> = OnceLock::new();
    *DABS.get_or_init(|| {
        std::env::var("PIGMENT_SMUDGE_SAMPLE_DABS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8)
    })
}

/// Whether the reference `update_smudge_color` would take a fresh sample on
/// this dab. Used to decide when to ask the device for the event sample.
#[inline]
fn would_resample(
    sv: &SettingValues,
    bucket: &SmudgeBucket,
    smudge_length: f32,
    smudge_mapping_active: bool,
) -> bool {
    if smudge_length >= 1.0 || !smudge_mapping_active {
        return false;
    }
    let factor = smudge_length.max(0.01);
    let recentness = bucket.recentness * factor;
    let threshold = ((0.5 * factor)
        .powf(sv.get(BrushSetting::SmudgeLengthLog))
        + 1e-16)
        .min(1.0);
    recentness < threshold
}

/// CPU `update_smudge_color` for one dab. `precomputed` carries an
/// event-level device sample; when present no canvas read or GPU sync happens.
#[allow(clippy::too_many_arguments)]
fn update_smudge_bucket_cpu<S: TiledSurface + ?Sized>(
    surface: &mut S,
    sv: &SettingValues,
    mut bucket: SmudgeBucket,
    px: f32,
    py: f32,
    draw_radius: f32,
    smudge_length: f32,
    smudge_mapping_active: bool,
    legacy_smudge: bool,
    precomputed: Option<RgbaF32>,
) -> (SmudgeBucket, bool) {
    let mut skip_dab = false;
    // libmypaint: enter update_smudge_color when
    //   smudge_length < 1.0 && (SMUDGE != 0 || mapping not constant)
    // — not gated on smudge_amt itself, so a curve-driven smudge
    // that hits 0 at one dab still decays its recentness counter
    // and stays in step with the reference.
    if smudge_length < 1.0 && smudge_mapping_active {
        let smudge_length_log = sv.get(BrushSetting::SmudgeLengthLog);
        let mut update_factor = smudge_length.max(0.01);

        // Decay the existing recentness; if it falls below the
        // libmypaint threshold we resample the canvas.
        let recentness = bucket.recentness * update_factor;
        bucket.recentness = recentness;
        // C: `MIN(1.0, powf(...) + margin)` (mypaint-brush.c:941).
        let threshold =
            ((0.5 * update_factor).powf(smudge_length_log) + 1e-16).min(1.0);

        let (sr, sg, sb, sa) = if recentness < threshold {
            // First call after a long pause initialises the
            // bucket directly with the sample.
            if recentness == 0.0 {
                update_factor = 0.0;
            }
            bucket.recentness = 1.0;

            let smudge_radius_log = sv.get(BrushSetting::SmudgeRadiusLog);
            // libmypaint feeds the post-radius_by_random radius
            // into update_smudge_color (`radius` at
            // mypaint-brush.c:1044, reassigned in the
            // radius_by_random branch a few lines earlier). For
            // brushes whose noise meaningfully shifts the radius
            // the smudge sample needs to scale with it.
            let smudge_radius = (draw_radius * smudge_radius_log.exp()).clamp(0.2, 1000.0);
            // libmypaint passes `legacy_smudge ? -1.0 : paint_factor`
            // to the surface and calls it with `ROUND(x), ROUND(y)`
            // (mypaint-brush.c:1128, :954). The sign of paint selects
            // legacy sampling inside get_color.
            let paint_for_sample = sv.get(BrushSetting::Paint);
            let sample = if let Some(s) = precomputed {
                s
            } else {
                // 采样必须看到当前 dab 之前的所有落笔：GPU 后端可能把
                // 之前的 draw_dab 排了队，先让它们落到画布上。
                surface.flush_dabs();
                let sx = round_c_pixel(px);
                let sy = round_c_pixel(py);
                if legacy_smudge {
                    surface.get_color(sx, sy, smudge_radius)
                } else {
                    surface.get_color_pigment(sx, sy, smudge_radius, paint_for_sample)
                }
            };

            if trace_get() {
                eprintln!("RAW x={:08x} y={:08x}", px.to_bits(), py.to_bits());
                eprintln!(
                    "GET x={:08x} y={:08x} r={:08x} pa={:08x} ro={:08x} go={:08x} bo={:08x} ao={:08x}",
                    round_c_pixel(px).to_bits(),
                    round_c_pixel(py).to_bits(),
                    smudge_radius.to_bits(),
                    paint_for_sample.to_bits(),
                    sample.r.to_bits(),
                    sample.g.to_bits(),
                    sample.b.to_bits(),
                    sample.a.to_bits(),
                );
            }

            // `smudge_transparency` gates the dab on the
            // sampled canvas alpha. Positive: skip when sample
            // is *more* transparent than the threshold;
            // negative: skip when *more* opaque than the
            // mirror threshold.
            let smudge_op_lim = sv.get(BrushSetting::SmudgeTransparency);
            if (smudge_op_lim > 0.0 && sample.a < smudge_op_lim)
                || (smudge_op_lim < 0.0 && sample.a > -smudge_op_lim)
            {
                // libmypaint RETURNS here (update_smudge_color
                // "return TRUE"), before the prev_col_* cache is
                // refreshed and before the bucket mix — the gated
                // sample must NOT leak into either. (recentness
                // was already reset to 1.0 above, like upstream.)
                skip_dab = true;
            } else {
                bucket.prev = [sample.r, sample.g, sample.b, sample.a];
            }
            (sample.r, sample.g, sample.b, sample.a)
        } else {
            (
                bucket.prev[0],
                bucket.prev[1],
                bucket.prev[2],
                bucket.prev[3],
            )
        };

        if !skip_dab {
            let fac = update_factor;
            let paint_factor = sv.get(BrushSetting::Paint);
            if !legacy_smudge {
                if sa > 0.01 {
                    let mixed = crate::spectral::mix_colors(
                        bucket.color,
                        [sr, sg, sb, sa],
                        fac,
                        paint_factor,
                    );
                    bucket.color = mixed;
                } else {
                    bucket.color[3] = (bucket.color[3] + sa) * 0.5;
                }
            } else {
                // Legacy smudge: SMUDGE_R += (1-fac)*a*r.
                let fac_new = (1.0 - fac) * sa;
                bucket.color[0] = bucket.color[0] * fac + sr * fac_new;
                bucket.color[1] = bucket.color[1] * fac + sg * fac_new;
                bucket.color[2] = bucket.color[2] * fac + sb * fac_new;
                bucket.color[3] =
                    (bucket.color[3] * fac + (1.0 - fac) * sa).clamp(0.0, 1.0);
            }
        }
    }
    (bucket, skip_dab)
}

#[allow(clippy::too_many_arguments)]
fn build_dab(
    brush: &Brush,
    sv: &SettingValues,
    state: &mut BrushState,
    px: f32,
    py: f32,
    // The (possibly radius_by_random-perturbed) dab radius — a LOCAL in
    // libmypaint's prepare_and_draw_dab, deliberately not read from
    // STATE.ACTUAL_RADIUS.
    radius: f32,
    smudge_amt: f32,
    // Per-dab opacity scaler from `radius_by_random`: when the noise
    // produced a larger radius, libmypaint scales `opaque` by
    // `(orig_radius / new_radius)²` to keep ink density even.
    opaque_scale: f32,
    smudge_bucket: SmudgeBucket,
    legacy_smudge: bool,
    // MyPaint passes `eotf() != 1.0` (default 2.2). When true, the brush
    // colour is transformed from sRGB to linear before it enters the dab
    // pipeline, and HSV/HSL dynamics run in gamma space.
    linear: bool,
) -> Dab {
    // libmypaint's `prepare_and_draw_dab` does the full colour
    // pipeline per dab in this exact order:
    //   brush BASEVAL(COLOR_*) → apply_smudge → eraser → HSV/HSL dynamics
    // Hokusai used to keep `state.actual_*` as a running accumulator and
    // do drift before smudge, which only matters when the brush actually
    // sets `change_color_*` AND `smudge` together — but several brushes
    // in the upstream pack do exactly that. Reordering here matches
    // libmypaint.
    // MyPaint stores brush colours as sRGB and lets the engine work in
    // linear sRGB (`lib/brush.py` `_transform_brush_color`). The app passes
    // sRGB HSV, so apply the transfer here when the linear path is active.
    let [base_r, base_g, base_b] = brush_base_linear(brush, linear);

    // 1) apply_smudge: derive eraser_target_alpha + mix the brush
    //    colour with the smudge bucket. See the libmypaint
    //    `apply_smudge` helper for the legacy / spectral split.
    let smudge_amt = smudge_amt.clamp(0.0, 1.0);
    let paint_factor = sv.get(BrushSetting::Paint);
    // C: `CLAMP((1.0 - smudge_factor) + smudge_factor * bucket_a, 0, 1)`
    // — the sum and clamp run in double and narrow once (mypaint-brush.c:1010).
    let smudge_alpha_product = smudge_amt * smudge_bucket.color[3];
    let eraser_target_alpha = ((1.0 - smudge_amt as f64) + smudge_alpha_product as f64)
        .clamp(0.0, 1.0) as f32;
    let mixed_rgb = if smudge_amt <= 0.0 || eraser_target_alpha <= 0.0 {
        [base_r, base_g, base_b]
    } else if !legacy_smudge {
        let brush_color = [base_r, base_g, base_b, 1.0];
        let mixed =
            crate::spectral::mix_colors(smudge_bucket.color, brush_color, smudge_amt, paint_factor);
        [
            mixed[0].clamp(0.0, 1.0),
            mixed[1].clamp(0.0, 1.0),
            mixed[2].clamp(0.0, 1.0),
        ]
    } else {
        let col_factor = 1.0 - smudge_amt;
        [
            ((smudge_amt * smudge_bucket.color[0] + col_factor * base_r) / eraser_target_alpha)
                .clamp(0.0, 1.0),
            ((smudge_amt * smudge_bucket.color[1] + col_factor * base_g) / eraser_target_alpha)
                .clamp(0.0, 1.0),
            ((smudge_amt * smudge_bucket.color[2] + col_factor * base_b) / eraser_target_alpha)
                .clamp(0.0, 1.0),
        ]
    };

    // 2) HSV / HSL color dynamics on the *post-smudge* colour. Matches
    //    libmypaint's order — running drift was already removed; this
    //    is the per-dab delta on whatever apply_smudge produced.
    let dh = sv.get(BrushSetting::ChangeColorH);
    let dv = sv.get(BrushSetting::ChangeColorV);
    let dhsv_s = sv.get(BrushSetting::ChangeColorHsvS);
    let dl = sv.get(BrushSetting::ChangeColorL);
    let dhsl_s = sv.get(BrushSetting::ChangeColorHslS);
    let mut color_r = mixed_rgb[0];
    let mut color_g = mixed_rgb[1];
    let mut color_b = mixed_rgb[2];
    let using_hsv_dynamics = dh != 0.0 || dhsv_s != 0.0 || dv != 0.0;
    let using_hsl_dynamics = dl != 0.0 || dhsl_s != 0.0;
    let using_color_dynamics = using_hsv_dynamics || using_hsl_dynamics;
    if linear && using_color_dynamics {
        // C: `powf(color, 1 / 2.2)` — the exponent is computed in double and
        // narrowed to float, which differs from `1.0f32 / 2.2f32` by 1 ULP.
        // C does not clamp before this call.
        let exponent = (1.0_f64 / 2.2_f64) as f32;
        color_r = color_r.powf(exponent);
        color_g = color_g.powf(exponent);
        color_b = color_b.powf(exponent);
    }
    if using_hsv_dynamics {
        let mut hsv = crate::color::rgb_to_hsv(color_r, color_g, color_b);
        hsv.h = (hsv.h + dh).rem_euclid(1.0);
        // libmypaint applies the saturation change with the *unclamped*
        // post-change value and only clamps inside hsv_to_rgb
        // (mypaint-brush.c:1166-1171).
        let v_raw = hsv.v + dv;
        hsv.s = (hsv.s + hsv.s * v_raw * dhsv_s).clamp(0.0, 1.0);
        hsv.v = v_raw.clamp(0.0, 1.0);
        let rgb = hsv_to_rgb(hsv);
        color_r = rgb.r;
        color_g = rgb.g;
        color_b = rgb.b;
    }
    if using_hsl_dynamics {
        let mut hsl = crate::color::rgb_to_hsl(color_r, color_g, color_b);
        // Same order: HSL saturation sees the unclamped lightness, then
        // hsl_to_rgb clamps both channels.
        let l_raw = hsl.l + dl;
        let edge = (1.0 - l_raw).abs().min(l_raw.abs());
        hsl.s = (hsl.s + hsl.s * edge * 2.0 * dhsl_s).clamp(0.0, 1.0);
        hsl.l = l_raw.clamp(0.0, 1.0);
        let rgb = crate::color::hsl_to_rgb(hsl);
        color_r = rgb.r;
        color_g = rgb.g;
        color_b = rgb.b;
    }
    if linear && using_color_dynamics {
        color_r = color_r.clamp(0.0, 1.0).powf(2.2);
        color_g = color_g.clamp(0.0, 1.0).powf(2.2);
        color_b = color_b.clamp(0.0, 1.0).powf(2.2);
    }

    // Diagnostic / test bookkeeping: the legacy `state.actual_*` fields
    // now record the last dab's post-dynamics colour.
    let hsv_out = crate::color::rgb_to_hsv(color_r, color_g, color_b);
    state.actual_h = hsv_out.h;
    state.actual_s = hsv_out.s;
    state.actual_v = hsv_out.v;

    let color = crate::color::RgbaF32 {
        r: color_r,
        g: color_g,
        b: color_b,
        a: 1.0,
    };

    // libmypaint composes the final opacity as opaque * opaque_multiply.
    // Many stock brushes (charcoal, pencil, …) drive opaque_multiply from
    // pressure, so skipping it makes them look wrong at non-full pressure.
    // libmypaint applies only MAX(0.0, ...) to OPAQUE before multiplying
    // with OPAQUE_MULTIPLY (mypaint-brush.c:1044-1047). The final product
    // is clamped to [0, 1] downstream. An explicit zero is a real zero.
    let opaque_setting = sv.get(BrushSetting::Opaque);
    let opaque_raw = opaque_setting.max(0.0);
    let opaque_mult = opaque_multiplier(sv);
    // NOTE: `opaque_scale` (radius_by_random's alpha correction) is applied
    // AFTER opaque_linearize below — libmypaint clamps and linearizes the
    // bare opaque*multiply product first (mypaint-brush.c:957-987) and only
    // multiplies the correction in at the radius_by_random branch (:1024).
    // The linearize curve is non-linear, so the order is visible.
    let mut opaque = (opaque_raw * opaque_mult).clamp(0.0, 1.0);
    if trace_opaque() {
        eprintln!(
            "OPA0 raw={:08x} fac={:08x} op={:08x} press={:08x}",
            opaque_setting.to_bits(), opaque_mult.to_bits(), opaque.to_bits(),
            state.pressure.to_bits(),
        );
    }

    // libmypaint's `opaque_linearize` compensates for the fact that
    // overlapping dabs accumulate alpha non-linearly: the per-dab alpha
    // is rooted by `1/dabs_per_pixel` so the *aggregate* opacity at the
    // dab center matches `opaque`. Brushes like the stock round brush
    // (`opaque_linearize=0.44`) rely on this to dim their feathered edges
    // without going full opaque — without it, hokusai's tails of a
    // pressure ramp keep painting at the unmodulated `opaque` value while
    // libmypaint's drop to near zero.
    let opaque_linearize = brush.get(BrushSetting::OpaqueLinearize).base_value.max(0.0);
    if opaque_linearize > 0.0 && opaque > 0.0 {
        // libmypaint's non-legacy path reads DABS_PER_* from STATE
        // (mypaint-brush.c:970) — committed from the *previous*
        // evaluation's SETTING values at the start of the current
        // simulation step, so the correction lags the curve by one dab
        // (and is absent entirely on the first dab after a reset,
        // where STATE is still 0 → dabs_per_pixel clamps to 1).
        let dpar = state.dabs_per_actual_radius;
        let dpbr = state.dabs_per_basic_radius;
        let mut dabs_per_pixel = (dpar + dpbr) * 2.0;
        if dabs_per_pixel < 1.0 {
            dabs_per_pixel = 1.0;
        }
        // C's `1.0 + ...` and `1.0 - ...` are double expressions narrowed
        // once to float; the powf exponent is likewise `(float)(1.0/dpp)`.
        dabs_per_pixel =
            (1.0 + opaque_linearize as f64 * (dabs_per_pixel as f64 - 1.0)) as f32;
        let beta = (1.0 - opaque as f64) as f32;
        let beta_dab = beta.powf((1.0 / dabs_per_pixel as f64) as f32);
        opaque = (1.0 - beta_dab as f64) as f32;
    }
    if trace_opaque() {
        eprintln!(
            "OPA1 op={:08x} ar={:08x} dpar={:?} dbr={:?}",
            opaque.to_bits(), state.actual_radius.to_bits(),
            state.dabs_per_actual_radius, state.dabs_per_basic_radius,
        );
    }
    // radius_by_random's `(orig/new)²` density compensation, post-linearize
    // like upstream.
    opaque *= opaque_scale;
    if trace_opaque() {
        eprintln!(
            "OPA2 corr={:08x} op={:08x} r={:08x}",
            opaque_scale.to_bits(), opaque.to_bits(), radius.to_bits(),
        );
    }

    let mut hardness = sv.get(BrushSetting::Hardness).clamp(0.0, 1.0);
    let mut radius = radius;

    // libmypaint's anti_aliasing: if the current edge fadeout (in pixels)
    // is narrower than the requested minimum, soften the brush by lowering
    // hardness and growing the geometric radius so the *optical* radius —
    // the perceptual center of the falloff — stays the same. Encoding AA
    // this way (rather than as a separate dab field) means the renderer
    // sees a regular hard/soft dab with no special path. See
    // libmypaint/mypaint-brush.c `prepare_and_draw_dab`.
    let aa_min = sv.get(BrushSetting::AntiAliasing).max(0.0);
    let current_fadeout = radius * (1.0 - hardness);
    if current_fadeout < aa_min {
        // libmypaint's literals (`1.0`, `2.0`) are double, so these
        // intermediates are evaluated in double before narrowing to float.
        let optical = (radius as f64
            - (1.0 - hardness as f64) * radius as f64 / 2.0) as f32;
        let hardness_new = ((optical as f64 - aa_min as f64 / 2.0)
            / (optical as f64 + aa_min as f64 / 2.0)) as f32;
        // libmypaint applies the result unconditionally; sub-pixel dabs end
        // up with negative hardness, which `draw_dab_default` rejects to
        // match the upstream `op->hardness == 0` early-out. We only guard
        // against `hardness_new == 1` here to avoid div-by-zero on the
        // radius assignment.
        if hardness_new < 1.0 {
            radius = (aa_min as f64 / (1.0 - hardness_new as f64)) as f32;
            hardness = hardness_new;
        }
    }

    // libmypaint's `snap_to_pixel`: pull the dab centre toward
    // (floor(x)+0.5, floor(y)+0.5) and quantise the radius to half-pixels
    // by the snap fraction. At snap=1.0 the dab lands exactly on a pixel
    // centre with a radius that doesn't bleed into a 4th neighbour.
    let mut px = px;
    let mut py = py;
    let snap = sv.get(BrushSetting::SnapToPixel).clamp(0.0, 1.0);
    if snap > 0.0 {
        let snapped_x = px.floor() + 0.5;
        let snapped_y = py.floor() + 0.5;
        px += (snapped_x - px) * snap;
        py += (snapped_y - py) * snap;
        let mut snapped_radius = (radius * 2.0).round() * 0.5;
        if snapped_radius < 0.5 {
            snapped_radius = 0.5;
        }
        if snap > 0.9999 {
            // libmypaint sheds a hair off the quantised radius so the
            // mask doesn't touch the 4th neighbour pixel through fp slop.
            snapped_radius -= 0.0001;
        }
        radius += (snapped_radius - radius) * snap;
    }

    // libmypaint folds the smudge-derived `eraser_target_alpha` into the
    // dab's source alpha BEFORE the eraser setting is applied: the
    // smudge bucket can already be partially transparent, and a smudge
    // brush is expected to "drag" that transparency along with the
    // colour. `alpha_eraser` is what the renderer multiplies the
    // per-pixel mask by, so passing the combined value here gives the
    // libmypaint blend.
    // C: `if (SETTING(ERASER)) eraser_target_alpha *= (1.0-SETTING(ERASER))`
    // — the literal is double, so the product rounds once at f32 narrowing.
    // The renderer clamps source alpha (mypaint-tiled-surface.c:656), so keep
    // the unclamped value.
    let eraser = sv.get(BrushSetting::Eraser);
    let alpha_eraser = if eraser != 0.0 {
        (eraser_target_alpha as f64 * (1.0 - eraser as f64)) as f32
    } else {
        eraser_target_alpha
    };
    if trace_eraser() {
        eprintln!(
            "ERA e={:08x} ta={:08x}",
            eraser.to_bits(),
            alpha_eraser.to_bits()
        );
    }

    if trace_angle() {
        eprintln!(
            "ANG set={:08x} st={:08x}",
            sv.get(BrushSetting::EllipticalDabAngle).to_bits(),
            state.actual_elliptical_angle.to_bits(),
        );
    }

    Dab {
        x: px,
        y: py,
        radius,
        color,
        opaque,
        hardness,
        softness: sv.get(BrushSetting::Softness).clamp(0.0, 1.0),
        alpha_eraser,
        // libmypaint draws with the carried STATE values — the angle is
        // the mod_arith-folded [-180, 0) representative, which matters for
        // f32 cos/sin rounding even though the ellipse is 180°-symmetric.
        aspect_ratio: state.actual_elliptical_ratio.max(1.0),
        angle: state.actual_elliptical_angle,
        lock_alpha: sv.get(BrushSetting::LockAlpha).clamp(0.0, 1.0),
        colorize: sv.get(BrushSetting::Colorize).clamp(0.0, 1.0),
        posterize: sv.get(BrushSetting::Posterize).clamp(0.0, 1.0),
        // libmypaint's `prepare_and_draw_dab` scales by 100 and clamps to
        // `[1, 128]` before handing the value to the posterize blend, so
        // the `.myb` setting `posterize_num = 0.02` (Posterizer) becomes a
        // 2-step quantisation rather than the 1-step degenerate hokusai
        // used to compute via `max(1.0)`.
        posterize_num: (sv.get(BrushSetting::PosterizeNum) * 100.0)
            .round()
            .clamp(1.0, 128.0),
        paint: sv.get(BrushSetting::Paint).clamp(0.0, 1.0),
        // AA has been baked into `radius` and `hardness` above.
        anti_aliasing: 0.0,
    }
}

/// libmypaint's per-speed input mapping. The brush's `speedN_gamma` setting
/// is `ln(gamma)`; with `gamma`, `m`, and `q` derived to anchor the curve at
/// `(speed=45, value=0.5)` with slope `0.015`, the resulting input is
/// `log(gamma + speed) * m + q`.
/// Port of libmypaint's `count_dabs_to` + `state_based_dab_count`: dabs to
/// draw to reach `(tgt_x, tgt_y)` over `dt_left` seconds. Reads (and seeds)
/// the carried STATE values exactly like upstream:
/// - `actual_radius` — 0 after a reset, in which case it's *written back*
///   as `base_radius` before use;
/// - `actual_elliptical_ratio/angle` — from the previous simulation step's
///   evaluation (plain euclidean distance until the first step has run);
/// - `dabs_per_*` — read directly from STATE, with no base-value fallback;
///   the first count after a reset therefore contributes only the
///   `dabs_per_second * dt` term, exactly like the C engine.
///
/// The C version ends with `if (isnan(res4) || res4 < 0.0) res4 = 0.0;`
/// (mypaint-brush.c:1282-1285); mirror that instead of inventing fallbacks.
fn count_dabs_to(
    brush: &Brush,
    state: &mut BrushState,
    cur_x: f32,
    cur_y: f32,
    tgt_x: f32,
    tgt_y: f32,
    dt_left: f32,
) -> f32 {
    let base_radius = brush
        .get(BrushSetting::Radius)
        .base_value
        .exp()
        .clamp(0.2, 1000.0);
    if state.actual_radius == 0.0 {
        state.actual_radius = base_radius;
    }

    let dx = tgt_x - cur_x;
    let dy = tgt_y - cur_y;
    let dist = if state.actual_elliptical_ratio > 1.0 {
        let angle_rad = radians_c(state.actual_elliptical_angle);
        // C calls the DOUBLE cos/sin and narrows the result to float
        // (`float cs = cos(angle_rad)`), not cosf/sinf.
        let cs = (angle_rad as f64).cos() as f32;
        let sn = (angle_rad as f64).sin() as f32;
        let yyr = (dy * cs - dx * sn) * state.actual_elliptical_ratio;
        let xxr = dy * sn + dx * cs;
        (yyr * yyr + xxr * xxr).sqrt()
    } else {
        dx.hypot(dy)
    };

    let res1 = dist / state.actual_radius * state.dabs_per_actual_radius;
    let res2 = dist / base_radius * state.dabs_per_basic_radius;
    let res3 = dt_left * state.dabs_per_second;
    let mut res = res1 + res2 + res3;
    if res.is_nan() || res < 0.0 {
        res = 0.0;
    }
    if trace_count() {
        eprintln!(
            "CNT x={:08x} y={:08x} x0={:08x} y0={:08x} dt={:08x} ra={:08x} dar={:08x} dbr={:08x} dps={:08x} dist={:08x} res1={:08x} res4={:08x}",
            tgt_x.to_bits(), tgt_y.to_bits(), cur_x.to_bits(), cur_y.to_bits(),
            dt_left.to_bits(), state.actual_radius.to_bits(),
            state.dabs_per_actual_radius.to_bits(), state.dabs_per_basic_radius.to_bits(),
            state.dabs_per_second.to_bits(), dist.to_bits(), res1.to_bits(), res.to_bits(),
        );
    }
    res
}


fn trace_count() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_COUNT").is_ok())
}

fn trace_stepdx() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_STEPDX").is_ok())
}

fn trace_dir() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_DIR").is_ok())
}

fn trace_get() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_GET").is_ok())
}

fn trace_inputbits() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_INPUTBITS").is_ok())
}

fn trace_angle() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_ANGLE").is_ok())
}

fn trace_eraser() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_ERASER").is_ok())
}

fn trace_opaque() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_OPAQUE").is_ok())
}

fn trace_pos() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_POS").is_ok())
}

fn trace_dabargs() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_DABARGS").is_ok())
}

/// Cached `HOKUSAI_TRACE_INPUTS` lookup: this runs inside the per-dab loop,
/// and `std::env::var` takes the process environment lock on every call.
fn trace_inputs() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_INPUTS").is_ok())
}

/// Port of libmypaint's `directional_offsets`. Sums the constant
/// `offset_x` / `offset_y` shift with up to six directional offsets:
/// one each (and a FLIP-mirrored partner) aligned with the smoothed
/// stroke direction, the pen ascension, and the view rotation. The
/// final pair is scaled by `base_radius * exp(offset_multiplier)` and
/// clamped to ±3240 px to match libmypaint's safety net against runaway
/// memory use from extreme settings.
///
#[allow(clippy::too_many_arguments)]
fn directional_offsets(
    sv: &SettingValues,
    base_radius: f32,
    flip: f32,
    direction_angle_dx: f32,
    direction_angle_dy: f32,
    ascension_deg: f32,
    viewrotation: f32,
) -> (f32, f32) {
    let offset_mult = sv.get(BrushSetting::OffsetMultiplier).exp();
    if !offset_mult.is_finite() {
        return (0.0, 0.0);
    }

    let mut dx = sv.get(BrushSetting::OffsetX);
    let mut dy = sv.get(BrushSetting::OffsetY);

    let offset_angle_adj = sv.get(BrushSetting::OffsetAngleAdj);
    // C: `fmodf(DEGREES(atan2f(dy, dx)) - 90, 360)` — fmod keeps the sign
    // (cos/sin only differ in rounding, not value, vs a euclidean mod) and
    // the degree conversion runs in double. Each branch below then narrows
    // RADIANS(...) to f32 and feeds it through the DOUBLE libm cos/sin —
    // exactly the C precision mix, which matters because these offsets get
    // multiplied by `base_radius * exp(offset_multiplier)` (often tens of
    // pixels) before landing on the canvas.
    let angle_deg =
        ((degrees_c(direction_angle_dy.atan2(direction_angle_dx)) - 90.0) as f32) % 360.0;

    #[inline]
    fn add_polar(dx: &mut f32, dy: &mut f32, angle: f32, factor: f32) {
        let a = angle as f64;
        *dx = (*dx as f64 + a.cos() * factor as f64) as f32;
        *dy = (*dy as f64 + a.sin() * factor as f64) as f32;
    }

    let offset_angle = sv.get(BrushSetting::OffsetAngle);
    if offset_angle != 0.0 {
        let a = radians_c(angle_deg + offset_angle_adj);
        add_polar(&mut dx, &mut dy, a, offset_angle);
    }

    let offset_angle_asc = sv.get(BrushSetting::OffsetAngleAsc);
    if offset_angle_asc != 0.0 {
        let a = radians_c(ascension_deg - viewrotation + offset_angle_adj);
        add_polar(&mut dx, &mut dy, a, offset_angle_asc);
    }

    let view_offset = sv.get(BrushSetting::OffsetAngleView);
    if view_offset != 0.0 {
        let a = radians_c(viewrotation + offset_angle_adj);
        add_polar(&mut dx, &mut dy, -a, view_offset);
    }

    let offset_dir_mirror = sv.get(BrushSetting::OffsetAngle2).max(0.0);
    if offset_dir_mirror != 0.0 {
        let a = radians_c(angle_deg + offset_angle_adj * flip);
        add_polar(&mut dx, &mut dy, a, offset_dir_mirror * flip);
    }

    let offset_asc_mirror = sv.get(BrushSetting::OffsetAngle2Asc).max(0.0);
    if offset_asc_mirror != 0.0 {
        let a = radians_c(ascension_deg - viewrotation + offset_angle_adj * flip);
        add_polar(&mut dx, &mut dy, a, flip * offset_asc_mirror);
    }

    let offset_view_mirror = sv.get(BrushSetting::OffsetAngle2View).max(0.0);
    if offset_view_mirror != 0.0 {
        let a = radians_c(viewrotation + offset_angle_adj);
        add_polar(&mut dx, &mut dy, -a, flip * offset_view_mirror);
    }

    const LIM: f32 = 3240.0;
    let base_mul = base_radius * offset_mult;
    (
        (dx * base_mul).clamp(-LIM, LIM),
        (dy * base_mul).clamp(-LIM, LIM),
    )
}

/// C `DEGREES(x)` — `((x) / (2*M_PI)) * 360.0` — evaluated in DOUBLE.
/// Callers narrow back to f32 at their own assignment points, exactly
/// like the C expression contexts.
#[inline]
fn degrees_c(x: f32) -> f64 {
    x as f64 / (2.0 * std::f64::consts::PI) * 360.0
}

/// C `ROUND(x)` from helpers.h: `(int)(x + 0.5)`, i.e. truncation toward
/// zero after the +0.5 bias (not Rust's half-away-from-zero `round()`).
#[inline]
fn round_c_pixel(v: f32) -> f32 {
    // `0.5` is a double literal in helpers.h, so the add happens in f64
    // and only then truncates. In f32 the add can round up across a .5
    // boundary and pick the neighbouring pixel.
    ((v as f64 + 0.5) as i32) as f32
}

/// C `RADIANS(x)` — `((x) * M_PI / 180.0)` in double, narrowed to f32 at
/// the assignment site like every C caller does.
#[inline]
fn radians_c(x: f32) -> f32 {
    (x as f64 * std::f64::consts::PI / 180.0) as f32
}

/// C `mod_arith(a, N)` (helpers.c:75): `a - N * floor(a / N)` — the
/// division happens in f32, `floor` promotes to double, and the final
/// multiply/subtract round once at the f32 narrowing. NOT the same
/// rounding as `f32::rem_euclid` (which is fmod-based).
#[inline]
fn mod_arith_c(a: f32, n: f32) -> f32 {
    (a as f64 - n as f64 * (a / n).floor() as f64) as f32
}

/// Smallest signed angular difference `b - a` (in degrees) — port of
/// helpers.c `smallest_angular_difference`, including its post-wrap
/// correction branch.
fn smallest_angular_diff(a: f32, b: f32) -> f32 {
    let d = b - a;
    let mut r = mod_arith_c(d + 180.0, 360.0) - 180.0;
    r += if r > 180.0 {
        -360.0
    } else if r < -180.0 {
        360.0
    } else {
        0.0
    };
    r
}

/// libmypaint's `INPUT(ATTACK_ANGLE)`: the smallest angular difference
/// between the pen's ascension direction and the stroke direction (offset
/// by 90°), both in degrees, wrapped to `(-180, 180]`.
fn attack_angle(ascension_deg: f32, dx_raw: f32, dy_raw: f32) -> f32 {
    // libmypaint: `smallest_angular_difference(STATE(ASCENSION),
    // mod_arith(DEGREES(dir_angle_360) + 90, 360))` — the signed angle
    // FROM the pen ascension TO (stroke direction + 90°). No
    // zero-direction special case: C's `atan2f(0, 0)` is 0, so a fresh
    // stroke starts at +90.
    let target = mod_arith_c((degrees_c(dy_raw.atan2(dx_raw)) + 90.0) as f32, 360.0);
    smallest_angular_diff(ascension_deg, target)
}

fn speed_input(speed_norm: f32, gamma_log: f32) -> f32 {
    // Bit-for-bit port of libmypaint's mapping
    // (settings_base_values_have_changed + the INPUT(SPEED*) line):
    // `gamma` and `m` are f32, but both `log` calls are the DOUBLE
    // precision libm `log` of an f32 sum, and the final expression is
    // evaluated in double before narrowing. The precision mix matters —
    // speed feeds curves whose outputs scale base_radius-sized offsets,
    // so a 1-ulp drift here visibly displaces scatter-brush dabs.
    let gamma = gamma_log.exp();
    let m = 0.015_f32 * (45.0 + gamma);
    let c1 = (((45.0_f32 + gamma) as f64).ln()) as f32;
    let q = 0.5_f32 - m * c1;
    (((gamma + speed_norm) as f64).ln() * m as f64 + q as f64) as f32
}

/// libmypaint's `INPUT(DIRECTION)` — 180°-folded direction in *degrees*,
/// corrected for the view rotation.
fn direction_input(dx: f32, dy: f32, viewrotation: f32) -> f32 {
    mod_arith_c(
        (degrees_c(dy.atan2(dx)) + viewrotation as f64 + 180.0) as f32,
        180.0,
    )
}

/// libmypaint's `INPUT(DIRECTION_ANGLE)` — full 360° direction in *degrees*,
/// corrected for the view rotation.
fn direction_angle(dx: f32, dy: f32, viewrotation: f32) -> f32 {
    // C: `fmodf(DEGREES(dir_angle_360) + viewrotation + 360.0, 360.0)` —
    // fmod, not euclidean mod (atan2 output + 360 is never negative
    // anyway).
    ((degrees_c(dy.atan2(dx)) + viewrotation as f64 + 360.0) as f32) % 360.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::SettingValue;

    struct CountingSurface {
        count: u32,
    }
    impl TiledSurface for CountingSurface {
        fn tile_request_start(&mut self, _tx: i32, _ty: i32) -> &mut crate::tile::TilePixels {
            unreachable!()
        }
        fn tile_request_end(&mut self, _tx: i32, _ty: i32) {}
        fn draw_dab(&mut self, _dab: &Dab) -> bool {
            self.count += 1;
            true
        }
    }

    fn make_brush(radius_log: f32, dabs_per_actual_radius: f32) -> Brush {
        let mut b = Brush::new();
        b.set(BrushSetting::Radius, SettingValue::constant(radius_log));
        b.set(
            BrushSetting::DabsPerActualRadius,
            SettingValue::constant(dabs_per_actual_radius),
        );
        b.set(BrushSetting::Opaque, SettingValue::constant(1.0));
        // libmypaint's from_defaults installs the opaque_multiply pressure
        // curve; tests exercise pressure 1.0, so a constant 1.0 is the
        // equivalent identity without pulling in the parser.
        b.set(BrushSetting::OpaqueMultiply, SettingValue::constant(1.0));
        b.set(BrushSetting::Hardness, SettingValue::constant(0.5));
        b
    }

    /// Drive the engine through libmypaint's two warm-up events: the first
    /// resets/seeds (no dabs), the second evaluates settings and commits
    /// `STATE.DABS_PER_*` (still no dabs, because the count was 0). A test
    /// that wants the *next* move to paint must call this first.
    fn warm_up<S: TiledSurface>(
        brush: &Brush,
        state: &mut BrushState,
        surf: &mut S,
        pressure: f32,
        xtilt: f32,
        ytilt: f32,
    ) {
        brush.stroke_to(state, surf, 0.0, 0.0, pressure, xtilt, ytilt, 10.0);
        brush.stroke_to(state, surf, 0.0, 0.0, pressure, xtilt, ytilt, 0.01);
    }

    #[test]
    fn no_movement_no_dabs() {
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        let painted = brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        assert!(!painted);
        assert_eq!(surf.count, 0);
    }

    #[test]
    fn moves_emit_proportional_dabs() {
        // 20 px / exp(1) ≈ 20 / 2.718 = 7.36 radii of travel. With DPAR=2
        // that's ~14.7 dabs per `count_dabs_to`. libmypaint's per-iteration
        // re-evaluation lands the integer count somewhere in this band.
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        warm_up(&brush, &mut state, &mut surf, 1.0, 0.0, 0.0);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        assert!(
            (12..=16).contains(&surf.count),
            "expected ~14 dabs, got {}",
            surf.count
        );
    }

    #[test]
    fn slow_tracking_smooths_position() {
        // High slow_tracking → fewer pixels covered → fewer dabs.
        let a = make_brush(1.0, 2.0);
        let mut b = make_brush(1.0, 2.0);
        b.set(BrushSetting::SlowTracking, SettingValue::constant(0.9));

        let mut sa = BrushState::default();
        let mut sb = BrushState::default();
        let mut surf_a = CountingSurface { count: 0 };
        let mut surf_b = CountingSurface { count: 0 };

        warm_up(&a, &mut sa, &mut surf_a, 1.0, 0.0, 0.0);
        a.stroke_to(&mut sa, &mut surf_a, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        warm_up(&b, &mut sb, &mut surf_b, 1.0, 0.0, 0.0);
        b.stroke_to(&mut sb, &mut surf_b, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);

        assert!(
            surf_b.count < surf_a.count,
            "slow_tracking should suppress dab count: {} >= {}",
            surf_b.count,
            surf_a.count
        );
    }

    #[test]
    fn stroke_threshold_drives_stroke_state_reset() {
        // libmypaint's stroke_threshold does *not* suppress dabs. It only
        // gates `STATE.STROKE_STARTED`: when pressure rises above the
        // threshold the stroke restarts (`stroke_state` → 0); when it falls
        // back below `threshold * 0.9` the started flag clears so the next
        // rise resets again.
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::StrokeThreshold, SettingValue::constant(0.5));
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        // Pressure below threshold: started stays false, but dabs still land.
        warm_up(&brush, &mut state, &mut surf, 0.3, 0.0, 0.0);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 0.3, 0.0, 0.0, 0.01);
        assert!(
            surf.count > 0,
            "stroke_threshold no longer gates dab emission"
        );
        assert!(
            !state.stroke_started,
            "0.3 < threshold 0.5, started stays off"
        );

        // Above threshold (after a seed pass): started flips on and
        // stroke_state restarts at 0.
        let mut s2 = BrushState::default();
        let mut surf2 = CountingSurface { count: 0 };
        // First call always goes through the seed branch.
        brush.stroke_to(&mut s2, &mut surf2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.01);
        s2.stroke_state = 0.7;
        brush.stroke_to(&mut s2, &mut surf2, 1.0, 0.0, 0.8, 0.0, 0.0, 0.01);
        assert!(
            s2.stroke_started,
            "pressure above threshold sets started=true"
        );
        // The rising edge resets stroke_state to 0 at the step where the
        // interpolated pressure crosses the threshold; the remaining steps
        // of the event then advance it again by `norm_dist * exp(-duration)`
        // (libmypaint does the same — the value never *ends* at exactly 0).
        // It must have restarted well below the pre-seeded 0.7, though.
        assert!(
            s2.stroke_state < 0.7,
            "rising-edge reset restarts stroke_state (got {})",
            s2.stroke_state
        );
    }

    #[test]
    fn tracking_noise_shifts_dab_positions() {
        // Same input + seed should be deterministic. Two states with the same
        // seed differ in the dab positions iff tracking_noise injects gauss.
        let noise_brush = {
            let mut b = make_brush(1.0, 2.0);
            b.set(BrushSetting::TrackingNoise, SettingValue::constant(0.5));
            b
        };
        let plain = make_brush(1.0, 2.0);

        struct CaptureSurface {
            xs: Vec<f32>,
        }
        impl TiledSurface for CaptureSurface {
            fn tile_request_start(&mut self, _: i32, _: i32) -> &mut crate::tile::TilePixels {
                unreachable!()
            }
            fn tile_request_end(&mut self, _: i32, _: i32) {}
            fn draw_dab(&mut self, d: &Dab) -> bool {
                self.xs.push(d.x);
                true
            }
        }

        let mut sa = BrushState::default();
        let mut sb = BrushState::default();
        let mut ca = CaptureSurface { xs: vec![] };
        let mut cb = CaptureSurface { xs: vec![] };

        plain.stroke_to(&mut sa, &mut ca, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        plain.stroke_to(&mut sa, &mut ca, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        plain.stroke_to(&mut sa, &mut ca, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        noise_brush.stroke_to(&mut sb, &mut cb, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        noise_brush.stroke_to(&mut sb, &mut cb, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        noise_brush.stroke_to(&mut sb, &mut cb, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);

        // Noise perturbs the segment length so dab counts can differ by one.
        // Compare overlapping prefixes — any difference proves noise applied.
        let any_differ = ca
            .xs
            .iter()
            .zip(cb.xs.iter())
            .any(|(a, b)| (a - b).abs() > 1e-3);
        assert!(
            any_differ || ca.xs.len() != cb.xs.len(),
            "tracking_noise should perturb the dab stream"
        );
    }

    #[test]
    fn speed_slowness_smooths_speed_input() {
        // High slowness → speed1_slow stays near 0 even after rapid event.
        let mut b = make_brush(1.0, 2.0);
        b.set(BrushSetting::Speed1Slowness, SettingValue::constant(0.99));
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        b.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        b.stroke_to(&mut state, &mut surf, 200.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let smoothed = state.norm_speed1_slow;

        let b2 = make_brush(1.0, 2.0); // slowness = 0 (default)
        let mut state2 = BrushState::default();
        let mut surf2 = CountingSurface { count: 0 };
        b2.stroke_to(&mut state2, &mut surf2, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        b2.stroke_to(&mut state2, &mut surf2, 200.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let raw = state2.norm_speed1_slow;

        assert!(
            smoothed < raw,
            "slowness should suppress speed1_slow ({smoothed} >= {raw})"
        );
    }

    #[test]
    fn tilt_declination_follows_libmypaint_convention() {
        // libmypaint: `tilt_declination = 90` when the pen is straight up,
        // dropping to ~30 at the steepest tilt (`90 - hypot(x, y) * 60`).
        // With a curve mapping declination 0 → 0 and 90 → 1, the upright
        // pose feeds the *larger* contribution to the radius curve, so the
        // tilted stroke should end up *smaller* than the upright one.
        let mut tilt_brush = make_brush(1.0, 2.0);
        tilt_brush.set(
            BrushSetting::Radius,
            SettingValue {
                base_value: 1.0,
                inputs: vec![crate::mapping::InputMapping {
                    input: BrushInput::TiltDeclination,
                    points: vec![(0.0, 0.0), (90.0, 1.0)],
                }],
                unknown_inputs: Default::default(),
            },
        );
        let mut s1 = BrushState::default();
        let mut surf1 = CountingSurface { count: 0 };
        tilt_brush.stroke_to(&mut s1, &mut surf1, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        tilt_brush.stroke_to(&mut s1, &mut surf1, 10.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let r_upright = s1.actual_radius;

        let mut s2 = BrushState::default();
        let mut surf2 = CountingSurface { count: 0 };
        tilt_brush.stroke_to(&mut s2, &mut surf2, 0.0, 0.0, 1.0, 1.0, 0.0, 0.01);
        tilt_brush.stroke_to(&mut s2, &mut surf2, 10.0, 0.0, 1.0, 1.0, 0.0, 0.01);
        let r_tilted = s2.actual_radius;

        assert!(
            r_upright > r_tilted,
            "upright pen has higher declination → bigger radius via curve: {r_upright} <= {r_tilted}"
        );
    }

    #[test]
    fn change_color_h_drifts_hue() {
        // libmypaint's HSV drift operates on `rgb_to_hsv(color)` — pure
        // black/grey has no defined hue, so the delta needs a saturated
        // base colour to be observable. We seed the brush as red and
        // verify the per-dab change_color_h actually rotates it.
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::ColorH, SettingValue::constant(0.0)); // red
        brush.set(BrushSetting::ColorS, SettingValue::constant(1.0));
        brush.set(BrushSetting::ColorV, SettingValue::constant(1.0));
        brush.set(BrushSetting::ChangeColorH, SettingValue::constant(0.5));
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        warm_up(&brush, &mut state, &mut surf, 1.0, 0.0, 0.0);
        let h0 = state.actual_h;
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let h1 = state.actual_h;
        assert!((h1 - h0).abs() > 0.01, "hue should drift, h0={h0}, h1={h1}");
    }

    struct DabSurface {
        last: Option<Dab>,
    }
    impl TiledSurface for DabSurface {
        fn tile_request_start(&mut self, _: i32, _: i32) -> &mut crate::tile::TilePixels {
            unreachable!()
        }
        fn tile_request_end(&mut self, _: i32, _: i32) {}
        fn draw_dab(&mut self, dab: &Dab) -> bool {
            self.last = Some(*dab);
            true
        }
    }

    fn draw_once(brush: &Brush, zoom: f32, rotation: f32, linear: bool) -> Dab {
        let mut state = BrushState::default();
        let mut surf = DabSurface { last: None };
        brush.stroke_to_full(
            &mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 10.0, zoom, rotation, 0.0, linear,
        );
        brush.stroke_to_full(
            &mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01, zoom, rotation, 0.0, linear,
        );
        brush.stroke_to_full(
            &mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01, zoom, rotation, 0.0, linear,
        );
        surf.last.expect("third event should paint")
    }

    #[test]
    fn viewzoom_changes_input_and_radius() {
        let mut brush = make_brush(1.0, 2.0);
        let mut radius = SettingValue::constant(1.0);
        radius.inputs.push(crate::mapping::InputMapping {
            input: BrushInput::Viewzoom,
            points: vec![(0.0, 0.0), (-2.0, 1.0)],
        });
        brush.set(BrushSetting::Radius, radius);

        let at_100 = draw_once(&brush, 1.0, 0.0, false).radius;
        let at_25 = draw_once(&brush, 0.25, 0.0, false).radius;
        assert!(
            at_25 > at_100 * 1.5,
            "viewzoom=-1.386 should raise radius: {at_25} vs {at_100}"
        );
    }

    #[test]
    fn linear_colour_converts_base_srgb_to_linear() {
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::ColorH, SettingValue::constant(0.0));
        brush.set(BrushSetting::ColorS, SettingValue::constant(0.0));
        brush.set(BrushSetting::ColorV, SettingValue::constant(0.5));

        let linear = draw_once(&brush, 1.0, 0.0, true);
        let srgb = draw_once(&brush, 1.0, 0.0, false);
        let expected = 0.5_f32.powf(2.2);
        assert!(
            (linear.color.r - expected).abs() < 1e-5,
            "linear base {} != {}",
            linear.color.r,
            expected
        );
        assert!((srgb.color.r - 0.5).abs() < 1e-5);
    }

    #[test]
    fn viewrotation_rotates_elliptical_dab_angle() {
        let brush = make_brush(1.0, 2.0);
        let upright = draw_once(&brush, 1.0, 0.0, false);
        let rotated = draw_once(&brush, 1.0, std::f32::consts::FRAC_PI_2, false);
        // Ellipse axes are 180°-symmetric; -180° and 0° are the same axis.
        let delta = (rotated.angle - upright.angle).rem_euclid(180.0);
        assert!(
            (delta - 90.0).abs() < 1e-3,
            "angle {} -> {}, delta {}",
            upright.angle,
            rotated.angle,
            delta
        );
    }

    #[test]
    fn long_stroke_requests_split() {
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let mut split = false;
        for i in 1..120 {
            brush.stroke_to_full(
                &mut state,
                &mut surf,
                i as f32 * 20.0,
                0.0,
                1.0,
                0.0,
                0.0,
                0.1,
                1.0,
                0.0,
                0.0,
                false,
            );
            if state.split_due {
                split = true;
                break;
            }
        }
        assert!(
            split,
            "stroke should request a split after 4+3*pressure seconds"
        );
    }

    #[test]
    fn opaque_multiply_explicit_zero_zeroes_the_dab() {
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::OpaqueMultiply, SettingValue::constant(0.0));
        let mut state = BrushState::default();
        let mut surf = DabSurface { last: None };
        warm_up(&brush, &mut state, &mut surf, 1.0, 0.0, 0.0);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let dab = surf.last.expect("move should emit a dab");
        assert_eq!(
            dab.opaque, 0.0,
            "explicit opaque_multiply=0 must not be treated as 1"
        );
    }

    #[test]
    fn pressure_above_one_is_not_clamped_by_the_engine() {
        // libmypaint clamps only `pressure <= 0`; the GUI clamps [0,1].
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        brush.stroke_to_full(
            &mut state, &mut surf, 0.0, 0.0, 2.0, 0.0, 0.0, 10.0, 1.0, 0.0, 0.0, false,
        );
        assert_eq!(state.pressure, 2.0);
        assert_eq!(state.last_pressure, 2.0);
    }

    #[test]
    fn dtime_exactly_five_seconds_is_not_a_reset() {
        let brush = make_brush(1.0, 2.0);
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        warm_up(&brush, &mut state, &mut surf, 1.0, 0.0, 0.0);
        state.smudge_bucket_mut(7).expect("bucket 7").color = [0.25, 0.5, 0.75, 1.0];

        // `dtime > max_dtime` is strict: matching upstream, exactly 5 s
        // keeps the stroke's state (including smudge buckets) alive.
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 5.0);
        assert_eq!(
            state.smudge_bucket(7).expect("bucket 7").color,
            [0.25, 0.5, 0.75, 1.0]
        );

        // 5.01 s takes the reset branch, which clears the buckets.
        brush.stroke_to(&mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 5.01);
        assert_eq!(state.smudge_bucket(7).expect("bucket 7").color, [0.0; 4]);
    }

    #[test]
    fn smudge_bucket_watermarks_track_selected_bucket() {
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::Smudge, SettingValue::constant(1.0));
        brush.set(BrushSetting::SmudgeLength, SettingValue::constant(0.5));
        brush.set(BrushSetting::SmudgeBucket, SettingValue::constant(7.0));
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        warm_up(&brush, &mut state, &mut surf, 1.0, 0.0, 0.0);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        assert_eq!(state.min_smudge_bucket_used(), 7);
        assert_eq!(state.max_smudge_bucket_used(), 7);
    }

    struct SamplingSurface {
        samples: std::cell::RefCell<Vec<(f32, f32)>>,
        pigment_calls: std::cell::Cell<u32>,
        legacy_calls: std::cell::Cell<u32>,
    }
    impl TiledSurface for SamplingSurface {
        fn tile_request_start(&mut self, _: i32, _: i32) -> &mut crate::tile::TilePixels {
            unreachable!()
        }
        fn tile_request_end(&mut self, _: i32, _: i32) {}
        fn draw_dab(&mut self, _: &Dab) -> bool {
            true
        }
        fn get_color(&self, x: f32, y: f32, _radius: f32) -> RgbaF32 {
            self.legacy_calls.set(self.legacy_calls.get() + 1);
            self.samples.borrow_mut().push((x, y));
            RgbaF32::new(0.0, 1.0, 0.0, 1.0)
        }
        fn get_color_pigment(&self, x: f32, y: f32, _radius: f32, _paint: f32) -> RgbaF32 {
            self.pigment_calls.set(self.pigment_calls.get() + 1);
            self.samples.borrow_mut().push((x, y));
            RgbaF32::new(1.0, 0.0, 0.0, 1.0)
        }
    }
    fn sampling_surface() -> SamplingSurface {
        SamplingSurface {
            samples: std::cell::RefCell::new(Vec::new()),
            pigment_calls: std::cell::Cell::new(0),
            legacy_calls: std::cell::Cell::new(0),
        }
    }

    #[test]
    fn smudge_sampling_uses_c_round_pixel_coordinates() {
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::Smudge, SettingValue::constant(1.0));
        brush.set(BrushSetting::SmudgeLength, SettingValue::constant(0.5));
        brush.set(BrushSetting::Paint, SettingValue::constant(0.0));
        let mut state = BrushState::default();
        let mut surf = sampling_surface();
        warm_up(&brush, &mut state, &mut surf, 1.0, 0.0, 0.0);
        brush.stroke_to(&mut state, &mut surf, 10.3, 10.7, 1.0, 0.0, 0.0, 0.01);
        let samples = surf.samples.borrow();
        assert!(!samples.is_empty(), "smudge brush must resample");
        for &(x, y) in samples.iter() {
            assert_eq!(x.fract(), 0.0, "sample x must be ROUND()ed: {x}");
            assert_eq!(y.fract(), 0.0, "sample y must be ROUND()ed: {y}");
        }
    }

    #[test]
    fn curve_driven_paint_mode_zero_uses_spectral_sampling() {
        // libmypaint's legacy_smudge requires a *constant* paint setting;
        // a mapping that evaluates to 0 still uses get_color_pigment.
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::Smudge, SettingValue::constant(1.0));
        brush.set(BrushSetting::SmudgeLength, SettingValue::constant(0.5));
        let mut paint = SettingValue::constant(0.0);
        paint.inputs.push(crate::mapping::InputMapping {
            input: BrushInput::Pressure,
            points: vec![(0.0, 0.0), (1.0, 0.0)],
        });
        brush.set(BrushSetting::Paint, paint);
        let mut state = BrushState::default();
        let mut surf = sampling_surface();
        warm_up(&brush, &mut state, &mut surf, 1.0, 0.0, 0.0);
        brush.stroke_to(&mut state, &mut surf, 10.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        assert!(surf.pigment_calls.get() > 0, "non-constant paint=0 is spectral");
        assert_eq!(surf.legacy_calls.get(), 0);
    }

    #[test]
    fn radius_by_random_uses_the_signed_setting() {
        let mut plus = make_brush(1.0, 2.0);
        plus.set(BrushSetting::RadiusByRandom, SettingValue::constant(0.3));
        let mut minus = make_brush(1.0, 2.0);
        minus.set(BrushSetting::RadiusByRandom, SettingValue::constant(-0.3));

        let r_plus = draw_once(&plus, 1.0, 0.0, false).radius;
        let r_minus = draw_once(&minus, 1.0, 0.0, false).radius;
        let base = 1.0_f32.exp();
        let product = r_plus * r_minus;
        assert!(
            (product - base * base).abs() < base * base * 0.02,
            "±radius_by_random must be reciprocal around the base radius: \
             {r_plus} * {r_minus} = {product}, expected {}",
            base * base
        );
    }

    #[test]
    fn opaque_multiply_above_one_can_reach_full_opacity() {
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::Opaque, SettingValue::constant(0.5));
        brush.set(BrushSetting::OpaqueMultiply, SettingValue::constant(2.0));
        let mut state = BrushState::default();
        let mut surf = DabSurface { last: None };
        warm_up(&brush, &mut state, &mut surf, 1.0, 0.0, 0.0);
        brush.stroke_to(&mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01);
        let dab = surf.last.expect("move should emit a dab");
        assert!(
            (dab.opaque - 1.0).abs() < 1e-6,
            "0.5 × 2 must clamp to 1.0, got {}",
            dab.opaque
        );
    }

    #[test]
    fn hsv_saturation_dynamics_use_unclamped_value() {
        // libmypaint computes `s += s * v * setting` before hsv_to_rgb
        // clamps v (mypaint-brush.c:1166-1171). With v_raw = 2 and a
        // negative saturation change this drives s to 0; pre-clamping v
        // to 1 would leave a tinted colour.
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::ColorH, SettingValue::constant(0.0));
        brush.set(BrushSetting::ColorS, SettingValue::constant(0.2));
        brush.set(BrushSetting::ColorV, SettingValue::constant(0.5));
        brush.set(BrushSetting::ChangeColorV, SettingValue::constant(1.5));
        brush.set(BrushSetting::ChangeColorHsvS, SettingValue::constant(-0.5));
        let dab = draw_once(&brush, 1.0, 0.0, false);
        assert!((dab.color.r - 1.0).abs() < 1e-5);
        assert!((dab.color.g - 1.0).abs() < 1e-5);
        assert!((dab.color.b - 1.0).abs() < 1e-5);
    }

    fn direction_dx_at_zoom(zoom: f32) -> f32 {
        let mut brush = make_brush(1.0, 2.0);
        brush.set(BrushSetting::DirectionFilter, SettingValue::constant(5.0));
        let mut state = BrushState::default();
        let mut surf = CountingSurface { count: 0 };
        brush.stroke_to_full(
            &mut state, &mut surf, 0.0, 0.0, 1.0, 0.0, 0.0, 10.0, zoom, 0.0, 0.0, false,
        );
        brush.stroke_to_full(
            &mut state, &mut surf, 20.0, 0.0, 1.0, 0.0, 0.0, 0.01, zoom, 0.0, 0.0, false,
        );
        state.direction_dx
    }

    #[test]
    fn direction_filter_uses_viewzoom_scaled_steps() {
        // libmypaint filters `step_dx * viewzoom`; at half zoom the same
        // document-space movement is half as much screen motion.
        let at_1 = direction_dx_at_zoom(1.0);
        let at_half = direction_dx_at_zoom(0.5);
        assert!(at_1 > 0.0 && at_half > 0.0);
        assert!(
            at_half < at_1 * 0.75,
            "half-zoom direction must lag more: {at_half} vs {at_1}"
        );
    }
}
