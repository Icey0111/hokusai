//! `TiledSurface` abstraction and the `Dab` description passed to it.
//!
//! Backends only need to implement tile lending; `draw_dab` and `get_color`
//! ship as default implementations so every backend gets identical pixels.
//! The defaults cover libmypaint's Normal+Eraser, Colorize, Posterize,
//! LockAlpha and spectral `paint` blend modes.

use crate::color::RgbaF32;
use crate::state::SmudgeBucket;
use crate::tile::TilePixels;

/// Deferred smudge work handed to a backend that can sample on the device.
///
/// The engine emits this instead of calling [`TiledSurface::get_color_pigment`]
/// when the backend advertises [`TiledSurface::defer_smudge`]. The backend is
/// responsible for running the whole `update_smudge_color` → `apply_smudge`
/// colour chain in submission order, so the sampled canvas already contains
/// the dabs queued before this one.
#[derive(Debug, Clone, Copy)]
pub struct DeferredSmudge {
    pub sample_x: f32,
    pub sample_y: f32,
    pub sample_radius: f32,
    /// `paint` forwarded to the sampler (`> 0` = pigment/WGM sampling).
    pub sample_paint: f32,
    pub smudge_amt: f32,
    pub smudge_length: f32,
    pub smudge_length_log: f32,
    /// `smudge_length < 1 && smudge is mapped` — the dab resamples/decays.
    pub update_active: bool,
    /// Whether this dab should take a fresh canvas sample. Event-level
    /// sampling sets this only for the first dab of a pointer event; later
    /// dabs reuse the previous sample while still mixing the bucket.
    pub resample: bool,
    pub paint_mode: f32,
    pub smudge_op_lim: f32,
    pub eraser: f32,
    /// Brush base colour after the linear/sRGB transform, RGB only.
    pub base_color: [f32; 3],
    pub bucket_index: u32,
    /// CPU mirror of the bucket; the backend uses it to seed a fresh chain.
    pub bucket: SmudgeBucket,
    /// Incremented by [`crate::BrushState::reset`] and by the engine's own
    /// reset path. A different value invalidates any cached device state.
    pub reset_epoch: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Dab {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub color: RgbaF32, // linear, straight alpha at this boundary
    pub opaque: f32,
    pub hardness: f32,
    /// libmypaint's `softness`: 0 = normal edge, 1 = draws nothing.
    pub softness: f32,
    pub alpha_eraser: f32,
    pub aspect_ratio: f32,
    pub angle: f32, // degrees
    pub lock_alpha: f32,
    pub colorize: f32,
    pub posterize: f32,
    pub posterize_num: f32,
    pub paint: f32,
    pub anti_aliasing: f32,
}

pub trait TiledSurface {
    fn tile_request_start(&mut self, tx: i32, ty: i32) -> &mut TilePixels;
    fn tile_request_end(&mut self, tx: i32, ty: i32);

    fn begin_atomic(&mut self) {}
    /// Returns the list of tiles modified since the last `begin_atomic`.
    fn end_atomic(&mut self) -> Vec<(i32, i32)> {
        Vec::new()
    }

    /// Flush any deferred dab work so subsequent reads see the latest pixels.
    ///
    /// Backends that render dabs on another device queue them between pointer
    /// events (and between reads of the canvas, e.g. smudge sampling) shall
    /// override this. The default implementation is a no-op.
    fn flush_dabs(&mut self) {}

    /// Whether [`Self::draw_dab_deferred_smudge`] can run the smudge colour
    /// chain on the backend. Returning `false` keeps the CPU sampling path.
    fn defer_smudge(&self) -> bool {
        false
    }

    /// Render a dab whose colour depends on a canvas sample that must be taken
    /// *after* all previously queued dabs. Returns `None` when the backend
    /// cannot express this dab; the engine then falls back to CPU sampling.
    ///
    /// `dab` carries the final geometry/opacity/softness. Its `color` and
    /// `alpha_eraser` are placeholders computed from the CPU bucket mirror and
    /// must be replaced by the backend with the result of `req`.
    fn draw_dab_deferred_smudge(
        &mut self,
        _dab: &Dab,
        _req: &DeferredSmudge,
    ) -> Option<bool> {
        None
    }

    /// Whether the device holds smudge bucket state newer than the CPU.
    ///
    /// Used to avoid a per-dab sync on brushes that never deferred anything.
    fn deferred_smudge_dirty(&self) -> bool {
        false
    }

    /// Make the CPU mirror of the smudge buckets authoritative again.
    ///
    /// Called before a CPU resample after any deferred work. Implementations
    /// flush queued dabs and, when `reset_epoch` still matches their device
    /// chain, copy the device bucket state into `buckets`. On an epoch
    /// mismatch the device state is discarded and `buckets` is left alone.
    fn sync_deferred_smudge(&mut self, _reset_epoch: u64, _buckets: &mut [SmudgeBucket]) {}

    /// Render one dab. Returns whether any pixel was modified.
    ///
    /// Default impl applies libmypaint's Normal+Eraser blend in linear sRGB
    /// fix15, plus colorize / posterize / lock_alpha / spectral `paint` when
    /// those dab fields are non-zero.
    fn draw_dab(&mut self, dab: &Dab) -> bool {
        crate::brushmodes::draw_dab_default(self, dab)
    }

    /// Read-only tile lookup, used by the default `get_color` and by any
    /// caller that wants to inspect the canvas without dirtying tiles.
    fn tile_lookup(&self, _tx: i32, _ty: i32) -> Option<&TilePixels> {
        None
    }

    /// Device-side pigment sample for event-level smudge. Returns `None` when
    /// the backend cannot sample on the GPU; callers fall back to
    /// [`Self::get_color_pigment`].
    #[allow(unused_variables)]
    fn get_color_pigment_gpu(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        paint: f32,
    ) -> Option<RgbaF32> {
        None
    }

    /// Average color within `radius` of `(x, y)`. Used by smudge / color
    /// picker.
    ///
    /// The default reads through [`Self::tile_lookup`] and silently returns
    /// a transparent result for backends that don't implement it. Backends
    /// without raw tile access (e.g. a single `Pixmap`-backed surface)
    /// can override and forward to
    /// [`crate::brushmodes::get_color_via_sample`] with their own
    /// per-pixel reader.
    fn get_color(&self, x: f32, y: f32, radius: f32) -> RgbaF32 {
        crate::brushmodes::get_color_default(self, x, y, radius)
    }

    /// libmypaint's `get_color_pigment`: alpha-weighted running WGM
    /// average in 10-channel spectral space, blended with the
    /// alpha-weighted linear average by `paint`. `paint < 0` falls
    /// back to plain [`Self::get_color`] (legacy sampling); `paint >=
    /// 0` runs the spectral path so blender / watercolour brushes mix
    /// the way libmypaint's `Surface2::get_color_pigment` does.
    fn get_color_pigment(&self, x: f32, y: f32, radius: f32, paint: f32) -> RgbaF32 {
        if paint < 0.0 {
            self.get_color(x, y, radius)
        } else {
            crate::brushmodes::get_color_pigment_default(self, x, y, radius, paint)
        }
    }
}
