//! Per-stroke mutable state. Mirrors libmypaint's `MyPaintBrush` runtime
//! fields so the stroke engine port can be a near-line-for-line translation.

use crate::rng::BrushRng;

/// libmypaint's `SMUDGE_BUCKET_SIZE` state block. MyPaint allocates 256 of
/// these; each dab picks one from `SETTING(SMUDGE_BUCKET)`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SmudgeBucket {
    /// `SMUDGE_R/G/B/A`: the filtered colour used by `apply_smudge`.
    pub color: [f32; 4],
    /// `PREV_COL_R/G/B/A`: the last raw canvas sample.
    pub prev: [f32; 4],
    /// `PREV_COL_RECENTNESS`: decays every dab; 1.0 means just resampled.
    pub recentness: f32,
}

/// MyPaint always creates brushes with 256 smudge buckets
/// (`lib/brush.hpp:26-27`).
pub const NUM_SMUDGE_BUCKETS: usize = 256;

#[derive(Debug, Clone)]
pub struct BrushState {
    // Smoothed input position.
    pub actual_x: f32,
    pub actual_y: f32,
    /// libmypaint's `STATE.ACTUAL_X` / `STATE.ACTUAL_Y` — the dab centre,
    /// which is lagged behind the slow-tracked cursor (`actual_x` here) by
    /// `slow_tracking_per_dab`. The two coincide when that setting is 0.
    pub actual_dab_x: f32,
    pub actual_dab_y: f32,

    // Last raw input event (for speed/direction calculation).
    pub last_event_x: f32,
    pub last_event_y: f32,
    pub last_event_time: f64,

    /// libmypaint `STATE(PRESSURE)`: the pressure after the last simulation
    /// step. The `dtime > 0.1` tablet warm-up path checks this before
    /// synthesising a zero-pressure event.
    pub pressure: f32,
    /// libmypaint `STATE(VIEWZOOM)`: canvas zoom factor, 1.0 = 100%.
    pub viewzoom: f32,
    /// libmypaint `STATE(VIEWROTATION)`: wrapped view rotation in degrees.
    pub viewrotation: f32,
    /// libmypaint `STATE(BARREL_ROTATION)`: stepped barrel rotation in degrees.
    pub barrel_rotation: f32,

    // Filtered radius (slow_tracking_per_dab applies here).
    pub actual_radius: f32,

    /// Set by `stroke_to` when the current segment should be committed and a
    /// new `Brushwork` started (`mypaint_brush_stroke_to` return value).
    pub split_due: bool,

    // Speed filter state — two parallel low-pass filters per libmypaint.
    pub norm_dx_slow: f32,
    pub norm_dy_slow: f32,
    pub norm_speed1_slow: f32,
    pub norm_speed2_slow: f32,
    /// Smoothed motion vector with 180° symmetry — libmypaint
    /// `STATE.DIRECTION_DX/DY`. Feeds `INPUT(DIRECTION)` which is
    /// mapped to `[0, 180)`. The 180° fold is done at update time:
    /// libmypaint picks the closest of `±(step_dx, step_dy)` so a
    /// stroke that flips back along itself doesn't oscillate.
    pub direction_dx: f32,
    pub direction_dy: f32,
    /// Smoothed motion vector without symmetry — libmypaint
    /// `STATE.DIRECTION_ANGLE_DX/DY`. Feeds `INPUT(DIRECTION_ANGLE)`
    /// (`[0, 360)`).
    pub direction_angle_dx: f32,
    pub direction_angle_dy: f32,
    /// libmypaint's `STATE.CUSTOM_INPUT` — the smoothed value of
    /// `SETTING(CUSTOM_INPUT)` with time constant `CUSTOM_INPUT_SLOWNESS`.
    /// Feeds `INPUT(CUSTOM)` so a brush can chain a curve's output back
    /// in as a (lagged) input on the next dab.
    pub custom_input: f32,
    /// libmypaint's `STATE.FLIP`: alternates `+1.0` / `-1.0` per dab so
    /// the `offset_angle_2*` settings can mirror the dab back and forth
    /// across the stroke direction (used by stamping / scatter brushes).
    /// `brush_reset` initialises it to `-1` so the first dab toggles to
    /// `+1` — matching the upstream comment.
    pub flip: f32,
    /// libmypaint's `tracking_noise` skip distance — coalesces incoming
    /// events until the cursor has travelled past `0.5 * noise * base_radius`
    /// pixels, at which point one new noise sample is consumed. Keeps
    /// noise-heavy brushes (DNA_brush, particle scatters, …) producing the
    /// same point density regardless of how often the app sends pointer
    /// events.
    pub skip_distance: f32,
    pub skip_last_x: f32,
    pub skip_last_y: f32,
    /// libmypaint's `skipped_dtime` is a `float`; the accumulator is narrowed
    /// on every skipped event, so matching the width matters for long
    /// tracking-noise drains.
    pub skipped_dtime: f32,
    /// libmypaint's `STATE.ASCENSION` — the dab-by-dab smoothed ascension
    /// angle (degrees). Advanced inside the dab loop by `frac *
    /// smallest_angular_difference(STATE.ASCENSION, target_ascension)`
    /// so directional offsets and `INPUT(TILT_ASCENSION)` see a lagged
    /// pen-rotation rather than a hard jump on each event.
    pub ascension: f32,
    /// libmypaint's `STATE.DECLINATION` — same idea as `ascension`, for
    /// the pen's tilt declination.
    pub declination: f32,
    /// libmypaint's `STATE.DECLINATIONX` / `DECLINATIONY`. Tilt
    /// components in degrees (`xtilt * 60`, `ytilt * 60`), smoothed
    /// per-dab so the per-axis input curves can ride on them.
    pub declination_x: f32,
    pub declination_y: f32,

    // Stroke accounting.
    pub stroke_total_painting_time: f64,
    pub stroke_current_idling_time: f64,
    /// libmypaint's `STATE.STROKE`. Accumulates `norm_dist * exp(-stroke_duration_logarithmic)`
    /// each dab and feeds `INPUT(STROKE)` (clamped to ≤ 1.0). Wrapped by
    /// `1 + stroke_holdtime`; once stroke_holdtime > 9.9 the value
    /// saturates at 1.0 until the stroke ends.
    pub stroke_state: f32,
    /// libmypaint's `STATE.STROKE_STARTED`. Flips on when `pressure`
    /// crosses above `stroke_threshold`, and off when it drops back below
    /// `stroke_threshold * 0.9`. On the rising edge the next dab resets
    /// `stroke_state` to 0 so the `Stroke` input starts a fresh ramp.
    pub stroke_started: bool,

    /// libmypaint's `STATE.DABS_PER_ACTUAL_RADIUS` / `_BASIC_RADIUS` /
    /// `_SECOND` — committed from the *previous* evaluation's SETTING values
    /// at the start of each simulation step (mypaint-brush.c:628-630), so
    /// `count_dabs_to` reads a one-step-delayed value. 0 after a reset,
    /// which triggers the base-value fallback in `state_based_dab_count`.
    pub dabs_per_actual_radius: f32,
    pub dabs_per_basic_radius: f32,
    pub dabs_per_second: f32,
    /// libmypaint's `STATE.ACTUAL_ELLIPTICAL_DAB_RATIO` / `_ANGLE` — set
    /// from the freshly evaluated SETTING at the end of each simulation
    /// step. `count_dabs_to` reads these (NOT the per-event evaluation),
    /// so the first count after a reset sees ratio 0 → plain euclidean
    /// distance.
    pub actual_elliptical_ratio: f32,
    pub actual_elliptical_angle: f32,
    /// Mirror of libmypaint's `settings_value[]` — the full output of the
    /// most recent per-dab setting evaluation. A handful of state commits
    /// read the *previous* evaluation (`STATE.DABS_PER_*`, the gridmap
    /// scale states), and `brush_reset` deliberately does NOT clear this
    /// (it lives outside `states[]`).
    pub prev_settings: crate::evaluator::SettingValues,

    // Distance accumulated since last dab (so dab count is fractional-stable).
    pub dist_past_dab: f32,
    pub last_dab_x: f32,
    pub last_dab_y: f32,
    pub last_dab_time: f64,

    // Painting color (HSV held independently; libmypaint's
    // change_color_* mutates these between dabs).
    pub actual_h: f32,
    pub actual_s: f32,
    pub actual_v: f32,

    /// libmypaint's per-brush smudge bucket array. `SETTING(SMUDGE_BUCKET)`
    /// selects one bucket per dab; each bucket carries its own filtered
    /// colour and `PREV_COL_*` sample cache.
    pub smudge_buckets: Vec<SmudgeBucket>,
    /// Low/high watermarks of the buckets actually selected since the last
    /// reset (`mypaint_brush_get_{min,max}_smudge_bucket_used`). `-1` means
    /// no bucket has been touched.
    pub min_smudge_bucket_used: i32,
    pub max_smudge_bucket_used: i32,
    /// Fallback smudge state used when a brush has zero buckets. MyPaint
    /// always allocates 256, so these stay unused, but libmypaint exposes
    /// them through `mypaint_brush_get/set_state` and we mirror that.
    pub smudge_state: [f32; 4],
    pub last_getcolor: [f32; 4],
    pub last_getcolor_recentness: f32,
    /// `STATE(RNG_SEED)`: libmypaint's update is commented out upstream, so
    /// this exists only for state introspection and stays 0.
    pub rng_seed: f32,
    /// `STATE(ATTACK_ANGLE)`: written by the stroke engine for introspection.
    pub attack_angle: f32,
    /// `STATE(GRIDMAP_X/Y)` — the gridmap coordinates committed each step
    /// before their (clamped) values are fed to `INPUT(GRIDMAP_*)`.
    pub gridmap_x: f32,
    pub gridmap_y: f32,

    pub rng: BrushRng,

    /// Pressure of the most recent stroke event. Used by `finish_stroke` to
    /// keep the trailing catch-up dabs at the same ink density the user was
    /// drawing with, rather than painting nothing at pressure=0.
    pub last_pressure: f32,
    /// Cached `INPUT(RANDOM)` value, mirroring libmypaint's
    /// `self->random_input`. libmypaint feeds the current value into
    /// every dab's setting evaluation and refreshes it from the PRNG
    /// *after* the dab is drawn, so consecutive dabs see distinct
    /// random samples without forcing the caller to re-read `next_unit`
    /// per setting query.
    pub random_input: f32,

    /// `false` until the first `stroke_to` has been processed. While `false`,
    /// `stroke_to` only seeds the position; no dabs are emitted. Mirrors
    /// libmypaint's "fresh stroke" handling.
    pub started: bool,
    /// Bumped every time the smudge buckets are cleared (this also happens in
    /// the engine's own `dtime > 5 s` reset path). Backends use it to invalidate
    /// device-side smudge state instead of syncing it back.
    pub reset_epoch: u64,
    /// Time accumulated since the last device smudge sample. Used to cap
    /// resampling at the display rate (240 Hz input otherwise asks for 4×
    /// more samples than can be shown).
    pub smudge_sample_dt: f64,
    pub smudge_sample_valid: bool,
    pub smudge_sample_color: [f32; 4],
    /// Dabs drawn since the last device smudge sample (dab-count gate keeps
    /// sample flushes small, unlike a time gate).
    pub smudge_dabs_since_sample: u32,
}

impl BrushState {
    pub fn new(seed: u32) -> Self {
        Self {
            actual_x: 0.0,
            actual_y: 0.0,
            actual_dab_x: 0.0,
            actual_dab_y: 0.0,
            last_event_x: 0.0,
            last_event_y: 0.0,
            last_event_time: 0.0,
            pressure: 0.0,
            viewzoom: 1.0,
            viewrotation: 0.0,
            barrel_rotation: 0.0,
            actual_radius: 0.0,
            split_due: false,
            norm_dx_slow: 0.0,
            norm_dy_slow: 0.0,
            norm_speed1_slow: 0.0,
            norm_speed2_slow: 0.0,
            direction_dx: 0.0,
            direction_dy: 0.0,
            direction_angle_dx: 0.0,
            direction_angle_dy: 0.0,
            custom_input: 0.0,
            flip: -1.0,
            skip_distance: 0.0,
            skip_last_x: 0.0,
            skip_last_y: 0.0,
            skipped_dtime: 0.0,
            ascension: 0.0,
            // libmypaint zero-initialises STATE via brush_reset's memset
            // (mypaint-brush.c:159) — DECLINATION starts at 0 and ramps
            // toward 90 (the no-tilt target) over the per-dab step
            // deltas of the first event. Hokusai used to seed this at
            // 90 outright, so curves keyed on tilt_declination saw the
            // saturated 90° value from the first dab instead of the
            // libmypaint ramp.
            declination: 0.0,
            declination_x: 0.0,
            declination_y: 0.0,
            stroke_total_painting_time: 0.0,
            stroke_current_idling_time: 0.0,
            stroke_state: 0.0,
            stroke_started: false,
            dabs_per_actual_radius: 0.0,
            dabs_per_basic_radius: 0.0,
            dabs_per_second: 0.0,
            actual_elliptical_ratio: 0.0,
            actual_elliptical_angle: 0.0,
            prev_settings: crate::evaluator::SettingValues::new(),
            dist_past_dab: 0.0,
            last_dab_x: 0.0,
            last_dab_y: 0.0,
            last_dab_time: 0.0,
            actual_h: 0.0,
            actual_s: 0.0,
            actual_v: 0.0,
            smudge_buckets: vec![SmudgeBucket::default(); NUM_SMUDGE_BUCKETS],
            min_smudge_bucket_used: -1,
            max_smudge_bucket_used: -1,
            smudge_state: [0.0; 4],
            last_getcolor: [0.0; 4],
            last_getcolor_recentness: 0.0,
            rng_seed: 0.0,
            attack_angle: 0.0,
            gridmap_x: 0.0,
            gridmap_y: 0.0,
            rng: BrushRng::new(seed),
            last_pressure: 0.0,
            random_input: 0.0,
            started: false,
            reset_epoch: 0,
            smudge_sample_dt: 0.0,
            smudge_sample_valid: false,
            smudge_sample_color: [0.0; 4],
            smudge_dabs_since_sample: 0,
        }
    }
}

impl BrushState {
    /// Reset back to the "no stroke in progress" state, preserving the PRNG
    /// stream so re-strokes are reproducible. This is libmypaint's
    /// `brush_reset`: it also clears the smudge buckets.
    pub fn reset(&mut self) {
        let mut fresh = Self::new(0);
        // Move the existing RNG into `fresh` so it survives the `*self =
        // fresh` assignment — no clone of the lagged-Fibonacci buffer.
        std::mem::swap(&mut fresh.rng, &mut self.rng);
        fresh.reset_epoch = self.reset_epoch.wrapping_add(1);
        *self = fresh;
    }

    /// libmypaint's `mypaint_brush_new_stroke`: reset only the two stroke
    /// timers. Position, RNG, and smudge buckets deliberately survive.
    pub fn new_stroke(&mut self) {
        self.stroke_current_idling_time = 0.0;
        self.stroke_total_painting_time = 0.0;
    }

    /// Read an internal state value (`mypaint_brush_get_state`).
    pub fn get_state(&self, field: BrushStateField) -> f32 {
        use BrushStateField::*;
        match field {
            X => self.actual_x,
            Y => self.actual_y,
            Pressure => self.pressure,
            PartialDabs => self.dist_past_dab,
            ActualRadius => self.actual_radius,
            SmudgeRa => self.smudge_state[0],
            SmudgeGa => self.smudge_state[1],
            SmudgeBa => self.smudge_state[2],
            SmudgeA => self.smudge_state[3],
            LastGetcolorR => self.last_getcolor[0],
            LastGetcolorG => self.last_getcolor[1],
            LastGetcolorB => self.last_getcolor[2],
            LastGetcolorA => self.last_getcolor[3],
            LastGetcolorRecentness => self.last_getcolor_recentness,
            ActualX => self.actual_dab_x,
            ActualY => self.actual_dab_y,
            NormDxSlow => self.norm_dx_slow,
            NormDySlow => self.norm_dy_slow,
            NormSpeed1Slow => self.norm_speed1_slow,
            NormSpeed2Slow => self.norm_speed2_slow,
            Stroke => self.stroke_state,
            StrokeStarted => {
                if self.stroke_started {
                    1.0
                } else {
                    0.0
                }
            }
            CustomInput => self.custom_input,
            RngSeed => self.rng_seed,
            ActualEllipticalDabRatio => self.actual_elliptical_ratio,
            ActualEllipticalDabAngle => self.actual_elliptical_angle,
            DirectionDx => self.direction_dx,
            DirectionDy => self.direction_dy,
            Declination => self.declination,
            Ascension => self.ascension,
            Viewzoom => self.viewzoom,
            Viewrotation => self.viewrotation,
            DirectionAngleDx => self.direction_angle_dx,
            DirectionAngleDy => self.direction_angle_dy,
            AttackAngle => self.attack_angle,
            Flip => self.flip,
            GridmapX => self.gridmap_x,
            GridmapY => self.gridmap_y,
            DeclinationX => self.declination_x,
            DeclinationY => self.declination_y,
            DabsPerBasicRadius => self.dabs_per_basic_radius,
            DabsPerActualRadius => self.dabs_per_actual_radius,
            DabsPerSecond => self.dabs_per_second,
            BarrelRotation => self.barrel_rotation,
        }
    }

    /// Write an internal state value (`mypaint_brush_set_state`).
    pub fn set_state(&mut self, field: BrushStateField, value: f32) {
        use BrushStateField::*;
        match field {
            X => self.actual_x = value,
            Y => self.actual_y = value,
            Pressure => self.pressure = value,
            PartialDabs => self.dist_past_dab = value,
            ActualRadius => self.actual_radius = value,
            SmudgeRa => self.smudge_state[0] = value,
            SmudgeGa => self.smudge_state[1] = value,
            SmudgeBa => self.smudge_state[2] = value,
            SmudgeA => self.smudge_state[3] = value,
            LastGetcolorR => self.last_getcolor[0] = value,
            LastGetcolorG => self.last_getcolor[1] = value,
            LastGetcolorB => self.last_getcolor[2] = value,
            LastGetcolorA => self.last_getcolor[3] = value,
            LastGetcolorRecentness => self.last_getcolor_recentness = value,
            ActualX => self.actual_dab_x = value,
            ActualY => self.actual_dab_y = value,
            NormDxSlow => self.norm_dx_slow = value,
            NormDySlow => self.norm_dy_slow = value,
            NormSpeed1Slow => self.norm_speed1_slow = value,
            NormSpeed2Slow => self.norm_speed2_slow = value,
            Stroke => self.stroke_state = value,
            StrokeStarted => self.stroke_started = value != 0.0,
            CustomInput => self.custom_input = value,
            RngSeed => self.rng_seed = value,
            ActualEllipticalDabRatio => self.actual_elliptical_ratio = value,
            ActualEllipticalDabAngle => self.actual_elliptical_angle = value,
            DirectionDx => self.direction_dx = value,
            DirectionDy => self.direction_dy = value,
            Declination => self.declination = value,
            Ascension => self.ascension = value,
            Viewzoom => self.viewzoom = value,
            Viewrotation => self.viewrotation = value,
            DirectionAngleDx => self.direction_angle_dx = value,
            DirectionAngleDy => self.direction_angle_dy = value,
            AttackAngle => self.attack_angle = value,
            Flip => self.flip = value,
            GridmapX => self.gridmap_x = value,
            GridmapY => self.gridmap_y = value,
            DeclinationX => self.declination_x = value,
            DeclinationY => self.declination_y = value,
            DabsPerBasicRadius => self.dabs_per_basic_radius = value,
            DabsPerActualRadius => self.dabs_per_actual_radius = value,
            DabsPerSecond => self.dabs_per_second = value,
            BarrelRotation => self.barrel_rotation = value,
        }
    }

    /// `mypaint_brush_get_smudge_bucket_state`: returns `None` when the
    /// index is outside the brush's bucket array.
    pub fn smudge_bucket(&self, index: usize) -> Option<&SmudgeBucket> {
        self.smudge_buckets.get(index)
    }

    /// Mutable bucket access without updating the used-watermarks (mirrors
    /// direct state manipulation).
    pub fn smudge_bucket_mut(&mut self, index: usize) -> Option<&mut SmudgeBucket> {
        self.smudge_buckets.get_mut(index)
    }

    /// `mypaint_brush_set_smudge_bucket_state`.
    #[allow(clippy::too_many_arguments)]
    pub fn set_smudge_bucket_state(
        &mut self,
        index: usize,
        r: f32,
        g: f32,
        b: f32,
        a: f32,
        prev_r: f32,
        prev_g: f32,
        prev_b: f32,
        prev_a: f32,
        prev_color_recentness: f32,
    ) -> bool {
        let Some(bucket) = self.smudge_buckets.get_mut(index) else {
            return false;
        };
        bucket.color = [r, g, b, a];
        bucket.prev = [prev_r, prev_g, prev_b, prev_a];
        bucket.recentness = prev_color_recentness;
        true
    }

    /// `mypaint_brush_get_smudge_bucket_state`, returned as
    /// `(color, prev, recentness)`.
    pub fn smudge_bucket_state(&self, index: usize) -> Option<([f32; 4], [f32; 4], f32)> {
        self.smudge_buckets
            .get(index)
            .map(|b| (b.color, b.prev, b.recentness))
    }

    /// `mypaint_brush_get_min_smudge_bucket_used`; `-1` if none.
    pub fn min_smudge_bucket_used(&self) -> i32 {
        self.min_smudge_bucket_used
    }

    /// `mypaint_brush_get_max_smudge_bucket_used`; `-1` if none.
    pub fn max_smudge_bucket_used(&self) -> i32 {
        self.max_smudge_bucket_used
    }
}

/// libmypaint's `MyPaintBrushState` enum, in `brushsettings.json` order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum BrushStateField {
    X = 0,
    Y,
    Pressure,
    PartialDabs,
    ActualRadius,
    SmudgeRa,
    SmudgeGa,
    SmudgeBa,
    SmudgeA,
    LastGetcolorR,
    LastGetcolorG,
    LastGetcolorB,
    LastGetcolorA,
    LastGetcolorRecentness,
    ActualX,
    ActualY,
    NormDxSlow,
    NormDySlow,
    NormSpeed1Slow,
    NormSpeed2Slow,
    Stroke,
    StrokeStarted,
    CustomInput,
    RngSeed,
    ActualEllipticalDabRatio,
    ActualEllipticalDabAngle,
    DirectionDx,
    DirectionDy,
    Declination,
    Ascension,
    Viewzoom,
    Viewrotation,
    DirectionAngleDx,
    DirectionAngleDy,
    AttackAngle,
    Flip,
    GridmapX,
    GridmapY,
    DeclinationX,
    DeclinationY,
    DabsPerBasicRadius,
    DabsPerActualRadius,
    DabsPerSecond,
    BarrelRotation,
}

impl BrushStateField {
    /// All 44 states in upstream enum order.
    pub const ALL: [Self; 44] = [
        Self::X,
        Self::Y,
        Self::Pressure,
        Self::PartialDabs,
        Self::ActualRadius,
        Self::SmudgeRa,
        Self::SmudgeGa,
        Self::SmudgeBa,
        Self::SmudgeA,
        Self::LastGetcolorR,
        Self::LastGetcolorG,
        Self::LastGetcolorB,
        Self::LastGetcolorA,
        Self::LastGetcolorRecentness,
        Self::ActualX,
        Self::ActualY,
        Self::NormDxSlow,
        Self::NormDySlow,
        Self::NormSpeed1Slow,
        Self::NormSpeed2Slow,
        Self::Stroke,
        Self::StrokeStarted,
        Self::CustomInput,
        Self::RngSeed,
        Self::ActualEllipticalDabRatio,
        Self::ActualEllipticalDabAngle,
        Self::DirectionDx,
        Self::DirectionDy,
        Self::Declination,
        Self::Ascension,
        Self::Viewzoom,
        Self::Viewrotation,
        Self::DirectionAngleDx,
        Self::DirectionAngleDy,
        Self::AttackAngle,
        Self::Flip,
        Self::GridmapX,
        Self::GridmapY,
        Self::DeclinationX,
        Self::DeclinationY,
        Self::DabsPerBasicRadius,
        Self::DabsPerActualRadius,
        Self::DabsPerSecond,
        Self::BarrelRotation,
    ];

    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }

    /// Same order as libmypaint's generated enum.
    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }
}

impl Default for BrushState {
    fn default() -> Self {
        // libmypaint seeds its per-brush PRNG with `1000`; matching it here
        // is necessary for byte-exact parity with the upstream goldens.
        Self::new(1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_enum_matches_libmypaint_count_and_order() {
        assert_eq!(BrushStateField::ALL.len(), 44);
        assert_eq!(BrushStateField::X.index(), 0);
        assert_eq!(BrushStateField::BarrelRotation.index(), 43);
        for (i, f) in BrushStateField::ALL.iter().enumerate() {
            assert_eq!(f.index(), i);
            assert_eq!(BrushStateField::from_index(i), Some(*f));
        }
    }

    #[test]
    fn every_state_field_roundtrips() {
        let mut st = BrushState::default();
        for (i, f) in BrushStateField::ALL.iter().enumerate() {
            let v = i as f32 + 0.25;
            st.set_state(*f, v);
            let got = st.get_state(*f);
            if *f == BrushStateField::StrokeStarted {
                assert_eq!(got, 1.0, "StrokeStarted is a boolean");
            } else {
                assert_eq!(got, v, "state {f:?} roundtrip");
            }
        }
    }

    #[test]
    fn new_stroke_only_resets_timers() {
        let mut st = BrushState::default();
        st.stroke_total_painting_time = 3.5;
        st.stroke_current_idling_time = 1.5;
        st.set_smudge_bucket_state(3, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9);
        st.min_smudge_bucket_used = 3;
        st.max_smudge_bucket_used = 3;

        st.new_stroke();
        assert_eq!(st.stroke_total_painting_time, 0.0);
        assert_eq!(st.stroke_current_idling_time, 0.0);
        assert_eq!(
            st.smudge_bucket_state(3),
            Some(([0.1, 0.2, 0.3, 0.4], [0.5, 0.6, 0.7, 0.8], 0.9))
        );
        assert_eq!(st.min_smudge_bucket_used(), 3);
        assert_eq!(st.max_smudge_bucket_used(), 3);
    }

    #[test]
    fn reset_clears_buckets_but_preserves_rng() {
        let mut st = BrushState::default();
        st.set_smudge_bucket_state(9, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0);
        st.min_smudge_bucket_used = 9;
        st.max_smudge_bucket_used = 9;

        let mut st2 = BrushState::default();
        let _ = st.rng.next_unit();
        let _ = st2.rng.next_unit();
        st.reset();
        // Both generators have consumed one draw; the reset state must
        // continue from the same stream, not reseed to 1000.
        assert_eq!(st.rng.next_unit(), st2.rng.next_unit());
        assert_eq!(st.smudge_bucket(9).unwrap().color, [0.0; 4]);
        assert_eq!(st.min_smudge_bucket_used(), -1);
        assert_eq!(st.max_smudge_bucket_used(), -1);
    }

    #[test]
    fn smudge_bucket_accessors_validate_index() {
        let mut st = BrushState::default();
        assert!(st.smudge_bucket(NUM_SMUDGE_BUCKETS).is_none());
        assert!(!st.set_smudge_bucket_state(
            NUM_SMUDGE_BUCKETS,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0
        ));
        assert!(st.set_smudge_bucket_state(0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0));
        assert_eq!(
            st.smudge_bucket_state(0),
            Some(([1.0, 0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], 1.0))
        );
    }
}
