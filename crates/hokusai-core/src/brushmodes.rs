//! Pixel blending — port of libmypaint's `brushmodes.c`.
//!
//! Coordinate / color conventions:
//! - Tile pixels are RGBA **fix15** (`u16` in `[0, 32768]`), **premultiplied**
//!   alpha, **linear sRGB**.
//! - Dab position `(x, y)` is in world-pixel space (sub-pixel f32).
//! - Dab `color` is straight-alpha linear sRGB; we premultiply at blend time.
//!
//! Implements libmypaint's `BlendMode_Normal_and_Eraser` plus the
//! `Colorize` / `Posterize` / `LockAlpha` overlays and the spectral
//! `paint` pigment-mixing passes.

#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

use crate::color::RgbaF32;
use crate::fix15::{self, FIX15_ONE};
use crate::surface::{Dab, TiledSurface};
use crate::tile::{TilePixels, TILE_SIZE};

/// Two-segment hardness falloff matching libmypaint's `calculate_opa`.
///
/// `rr` is squared normalized distance (`r² / radius²`). Returns 0 outside
/// the dab. At `rr = hardness` the two segments meet at value `hardness`,
/// giving a smooth ramp from 1 at center to 0 at the edge.
#[inline]
fn opa_at(rr: f32, hardness: f32, softness: f32) -> f32 {
    if rr > 1.0 {
        return 0.0;
    }
    // C precomputes the softness-scaled offsets/slopes and then does
    // `opa += rr * fac`. Applying `(1 - softness)` afterwards is
    // mathematically equal but changes the last bit for edge pixels.
    let one_minus_softness = 1.0 - softness;
    if hardness >= 1.0 {
        return one_minus_softness;
    }
    if hardness <= 0.0 {
        return (1.0 - rr) * one_minus_softness;
    }
    let segment1_offset = one_minus_softness;
    let segment1_slope = -(1.0 / hardness - 1.0) * one_minus_softness;
    let segment2_offset = hardness / (1.0 - hardness) * one_minus_softness;
    let segment2_slope = -hardness / (1.0 - hardness) * one_minus_softness;
    let (mut opa, fac) = if rr <= hardness {
        (segment1_offset, segment1_slope)
    } else {
        (segment2_offset, segment2_slope)
    };
    opa += rr * fac;
    opa
}

/// Compute squared normalized distance from dab center, accounting for
/// `aspect_ratio` (≥1) and `angle` (degrees).
#[inline]
fn rr_at(px: f32, py: f32, x: f32, y: f32, aspect: f32, cs: f32, sn: f32, inv_r2: f32) -> f32 {
    // libmypaint uses pixel center coordinates (px + 0.5, py + 0.5).
    let yy = py + 0.5 - y;
    let xx = px + 0.5 - x;
    let yyr = (yy * cs - xx * sn) * aspect;
    let xxr = yy * sn + xx * cs;
    (yyr * yyr + xxr * xxr) * inv_r2
}

/// libmypaint's `calculate_r_sample` — squared elliptical distance from
/// the dab centre (in pixel-relative coordinates, not normalised).
#[inline]
fn r_sample(x: f32, y: f32, aspect: f32, sn: f32, cs: f32) -> f32 {
    let yyr = (y * cs - x * sn) * aspect;
    let xxr = y * sn + x * cs;
    yyr * yyr + xxr * xxr
}

/// libmypaint's `sign_point_in_line`.
#[inline]
fn sign_point_in_line(px: f32, py: f32, vx: f32, vy: f32) -> f32 {
    (px - vx) * (-vy) - vx * (py - vy)
}

/// libmypaint's `closest_point_to_line` — orthogonal projection onto
/// the line through the origin spanned by `(lx, ly)`.
#[inline]
fn closest_point_to_line(lx: f32, ly: f32, px: f32, py: f32) -> (f32, f32) {
    let l2 = lx * lx + ly * ly;
    let dot = px * lx + py * ly;
    let t = dot / l2;
    (lx * t, ly * t)
}

/// libmypaint's `calculate_rr_antialiased`. Returns an AA-corrected
/// squared normalised distance for sub-pixel edge fading. Used for
/// dabs with `radius < 3` where the plain `rr_at` value would alias
/// hard at the dab boundary.
#[inline]
fn rr_at_aa(
    px: f32,
    py: f32,
    x: f32,
    y: f32,
    aspect: f32,
    cs: f32,
    sn: f32,
    inv_r2: f32,
    r_aa_start: f32,
) -> f32 {
    let pixel_right = x - px;
    let pixel_bottom = y - py;
    let pixel_center_x = pixel_right - 0.5;
    let pixel_center_y = pixel_bottom - 0.5;
    let pixel_left = pixel_right - 1.0;
    let pixel_top = pixel_bottom - 1.0;

    let (nearest_x, nearest_y, rr_near);
    if pixel_left < 0.0 && pixel_right > 0.0 && pixel_top < 0.0 && pixel_bottom > 0.0 {
        nearest_x = 0.0;
        nearest_y = 0.0;
        rr_near = 0.0;
    } else {
        let (nx, ny) = closest_point_to_line(cs, sn, pixel_center_x, pixel_center_y);
        nearest_x = nx.clamp(pixel_left, pixel_right);
        nearest_y = ny.clamp(pixel_top, pixel_bottom);
        let r_near = r_sample(nearest_x, nearest_y, aspect, sn, cs);
        rr_near = r_near * inv_r2;
    }

    if rr_near > 1.0 {
        return rr_near;
    }

    let center_sign = sign_point_in_line(pixel_center_x, pixel_center_y, cs, -sn);
    let rad_area_1 = (1.0_f32 / core::f32::consts::PI).sqrt();
    let (farthest_x, farthest_y) = if center_sign < 0.0 {
        (nearest_x - sn * rad_area_1, nearest_y + cs * rad_area_1)
    } else {
        (nearest_x + sn * rad_area_1, nearest_y - cs * rad_area_1)
    };

    let r_far = r_sample(farthest_x, farthest_y, aspect, sn, cs);
    let rr_far = r_far * inv_r2;

    if r_far < r_aa_start {
        return (rr_far + rr_near) * 0.5;
    }

    let visibility_near = 1.0 - rr_near;
    let delta = rr_far - rr_near;
    let delta2 = 1.0 + delta;
    let visibility_near_norm = visibility_near / delta2;
    1.0 - visibility_near_norm
}

/// Cached `HOKUSAI_TRACE_DABS` lookup — `std::env::var` on the dab hot path
/// is surprisingly expensive on Windows (environment lock + allocation), and
/// it used to run once per dab.
fn trace_mask() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_MASK").is_ok())
}

fn trace_sample() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_SAMPLE").is_ok())
}

fn trace_dabs() -> bool {
    use std::sync::OnceLock;
    static TRACE: OnceLock<bool> = OnceLock::new();
    *TRACE.get_or_init(|| std::env::var("HOKUSAI_TRACE_DABS").is_ok())
}

/// Per-dab geometry shared by every row of every tile.
#[derive(Clone, Copy)]
struct DabGeometry {
    r_fringe: f32,
    /// 椭圆（含 1px fringe）在 y 方向的半轴：`(r+1) * sqrt(sn² + (cs/aspect)²)`。
    /// 行的有效范围只到这里，不再用方形包围盒的 `r_fringe`。
    hy_fringe: f32,
    aspect: f32,
    cs: f32,
    sn: f32,
    inv_r2: f32,
    r_aa_start: f32,
    hardness: f32,
    softness: f32,
}

/// Per-dab pass parameters. Everything that used to be recomputed once per
/// tile (source spectral upsampling, pass opacities, etc.) is derived here so
/// each row only does the per-pixel work.
#[derive(Clone, Copy)]
struct DabPasses {
    src_r: u32,
    src_g: u32,
    src_b: u32,
    color_a: u32,
    normal_legacy: Option<u32>,
    lock_alpha_legacy: Option<u32>,
    normal_paint: Option<u32>,
    lock_alpha_paint: Option<u32>,
    colorize_op: Option<u32>,
    posterize_op: Option<u32>,
    posterize_num: u32,
    spec_a_log: [f32; 10],
    /// `fastpow(spec_a[i], 0)`，仅 `color_a == 0` 时使用。
    ///
    /// 纯擦除 dab（刮刀在空画布上、smudge 尚未取到颜色）的源色指数恒为 0，
    /// WGM 只剩 `spec_b^1` 一侧，可以省掉一半 `fastpow`，结果仍逐位相同。
    spec_a_zero_pow: [f32; 10],
    /// AVX2 batch pigment blending is available (x86_64 with runtime support).
    simd: bool,
}

impl DabPasses {
    fn new(dab: &Dab) -> Self {
        let src = clamp_color(dab.color);
        let src_r = (src.r * FIX15_ONE as f32) as u32;
        let src_g = (src.g * FIX15_ONE as f32) as u32;
        let src_b = (src.b * FIX15_ONE as f32) as u32;
        let color_a = dab.alpha_eraser.clamp(0.0, 1.0);
        let color_a_fix = (color_a * FIX15_ONE as f32) as u32;
        let opaque_f = dab.opaque.clamp(0.0, 1.0);
        let lock_alpha = dab.lock_alpha.clamp(0.0, 1.0);
        let colorize = dab.colorize.clamp(0.0, 1.0);
        let posterize = dab.posterize.clamp(0.0, 1.0);
        let posterize_num = dab.posterize_num.clamp(1.0, 128.0) as u32;
        let paint = dab.paint.clamp(0.0, 1.0);
        // process_op: `op->normal = (1-lock_alpha)(1-colorize)(1-posterize)`.
        let normal_f = (1.0 - lock_alpha) * (1.0 - colorize) * (1.0 - posterize);
        // C passes each blend's opacity as `float-product * (1<<15)` into a
        // uint16_t parameter — truncation happens exactly once, here.
        let fix = |v: f32| -> u32 { (v * FIX15_ONE as f32) as u32 };

        let mut normal_legacy = None;
        let mut lock_alpha_legacy = None;
        let mut normal_paint = None;
        let mut lock_alpha_paint = None;
        let mut colorize_op = None;
        let mut posterize_op = None;

        if paint < 1.0 {
            if normal_f != 0.0 {
                normal_legacy = Some(fix(normal_f * opaque_f * (1.0 - paint)));
            }
            if lock_alpha != 0.0 && color_a != 0.0 {
                lock_alpha_legacy = Some(fix(lock_alpha
                    * opaque_f
                    * (1.0 - colorize)
                    * (1.0 - posterize)
                    * (1.0 - paint)));
            }
        }
        if paint > 0.0 {
            if normal_f != 0.0 {
                normal_paint = Some(fix(normal_f * opaque_f * paint));
            }
            if lock_alpha != 0.0 && color_a != 0.0 {
                lock_alpha_paint = Some(fix(lock_alpha
                    * opaque_f
                    * (1.0 - colorize)
                    * (1.0 - posterize)
                    * paint));
            }
        }
        if colorize != 0.0 {
            colorize_op = Some(fix(colorize * opaque_f));
        }
        if posterize != 0.0 {
            posterize_op = Some(fix(posterize * opaque_f));
        }

        let mut spec_a_log = [0.0f32; 10];
        let mut spec_a_zero_pow = [0.0f32; 10];
        if paint > 0.0 {
            let spec_a = crate::spectral::rgb_to_spectral(
                src_r as f32 / FIX15_ONE as f32,
                src_g as f32 / FIX15_ONE as f32,
                src_b as f32 / FIX15_ONE as f32,
            );
            for i in 0..10 {
                spec_a_log[i] = crate::spectral::fastlog2(spec_a[i]);
            }
            if color_a == 0.0 {
                for i in 0..10 {
                    spec_a_zero_pow[i] = crate::spectral::fastpow_from_log(spec_a_log[i], 0.0);
                }
            }
        }

        #[cfg(target_arch = "x86_64")]
        let simd = crate::spectral::avx2_available();
        #[cfg(not(target_arch = "x86_64"))]
        let simd = false;

        Self {
            src_r,
            src_g,
            src_b,
            color_a: color_a_fix,
            normal_legacy,
            lock_alpha_legacy,
            normal_paint,
            lock_alpha_paint,
            colorize_op,
            posterize_op,
            posterize_num,
            spec_a_log,
            spec_a_zero_pow,
            simd,
        }
    }
}

/// 计算一行里 mask 非零的像素，追加到 `out`；`base` 是该行第一个像素在
/// 目标扁平像素数组里的下标。返回追加数量。
///
/// 椭圆在**这一行**的 x 范围由解析解给出：把 `rr(px, yp) ≤ 1` 解成关于 xx
/// 的二次式。libmypaint 用方形包围盒逐像素算 rr，对 aspect 5–6.5 的刮刀，
/// 窄轴方向绝大多数像素的 mask 都是 0。这里把范围收紧到椭圆内（半径取
/// `r_fringe`，额外再放 1px 防止浮点边界漏像素），跳过的一定是 mask=0 的
/// 像素，输出逐位不变。
fn mask_row_into(
    out: &mut [(usize, u32)],
    base: usize,
    xl: f32,
    yl: f32,
    yp: f32,
    geom: &DabGeometry,
) -> usize {
    let x0 = ((xl - geom.r_fringe).floor() as i32).max(0);
    let x1 = ((xl + geom.r_fringe).floor() as i32).min(TILE_SIZE as i32 - 1);
    if x0 > x1 {
        return 0;
    }
    let (row_x0, row_x1) = if geom.r_aa_start >= 0.0 {
        // Sub-3px dabs use the antialiased coverage function, whose
        // farthest-point sampling reaches outside the geometric ellipse
        // (up to rad_area_1 ≈ 0.564 px). The ellipse-tightened range would
        // drop those edge pixels, so use libmypaint's square bbox.
        (x0, x1)
    } else {
        let yy = yp as f64 + 0.5 - yl as f64;
        let aspect = geom.aspect as f64;
        let cs = geom.cs as f64;
        let sn = geom.sn as f64;
        let a = sn * sn * aspect * aspect + cs * cs;
        let b = 2.0 * yy * sn * cs * (1.0 - aspect * aspect);
        let c = yy * yy * (cs * cs * aspect * aspect + sn * sn)
            - (geom.r_fringe as f64) * (geom.r_fringe as f64);
        let disc = b * b - 4.0 * a * c;
        if disc < 0.0 || a <= 0.0 {
            return 0;
        }
        let root = disc.sqrt();
        let xx0 = (-b - root) / (2.0 * a);
        let xx1 = (-b + root) / (2.0 * a);
        let lo = (xx0 + xl as f64 - 0.5).floor() as i32 - 1;
        let hi = (xx1 + xl as f64 - 0.5).ceil() as i32 + 1;
        (lo.max(x0), hi.min(x1))
    };
    if row_x0 > row_x1 {
        return 0;
    }
    let mut n = 0usize;
    for xp in row_x0..=row_x1 {
        let rr = if geom.r_aa_start >= 0.0 {
            rr_at_aa(
                xp as f32,
                yp,
                xl,
                yl,
                geom.aspect,
                geom.cs,
                geom.sn,
                geom.inv_r2,
                geom.r_aa_start,
            )
        } else {
            rr_at(
                xp as f32,
                yp,
                xl,
                yl,
                geom.aspect,
                geom.cs,
                geom.sn,
                geom.inv_r2,
            )
        };
        let opa = opa_at(rr, geom.hardness, geom.softness);
        let mask = (opa * FIX15_ONE as f32) as u32;
        if mask != 0 {
            out[n] = (base + xp as usize, mask);
            n += 1;
        }
    }
    n
}

/// 对一组 `(扁平像素下标, mask)` 执行 dab 的全部混合 pass。
///
/// 像素之间互不影响，所以把整块 tile（或整行）的像素汇总后一次执行，
/// 与逐行执行的结果逐位相同；但循环长度从 ~5 像素变成上百像素，
/// 自动向量化才真正生效（细长刮刀此前每行只有 5–6 个像素）。
#[inline]
fn blend_entries(pixels: &mut [[u16; 4]], entries: &[(usize, u32)], passes: &DabPasses) -> bool {
    if entries.is_empty() {
        return false;
    }
    if trace_mask() {
        static CALLS: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(0);
        if CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            for &(idx, mask) in entries {
                eprintln!("MSK {} {} {}", idx % TILE_SIZE, idx / TILE_SIZE, mask);
            }
        }
    }
    if let Some(opacity) = passes.normal_legacy {
        if passes.color_a == FIX15_ONE {
            blend_normal(pixels, entries, passes, opacity);
        } else {
            blend_normal_and_eraser(pixels, entries, passes, passes.color_a, opacity);
        }
    }
    if let Some(opacity) = passes.lock_alpha_legacy {
        blend_lock_alpha(pixels, entries, passes, opacity);
    }
    if let Some(opacity) = passes.normal_paint {
        if passes.color_a == FIX15_ONE {
            blend_normal_paint(pixels, entries, passes, opacity);
        } else if passes.color_a == 0 {
            blend_eraser_only_paint(pixels, entries, passes, opacity);
        } else {
            blend_normal_and_eraser_paint(pixels, entries, passes, passes.color_a, opacity);
        }
    }
    if let Some(opacity) = passes.lock_alpha_paint {
        blend_lock_alpha_paint(pixels, entries, passes, opacity);
    }
    if let Some(opacity) = passes.colorize_op {
        blend_color(pixels, entries, passes, opacity);
    }
    if let Some(opacity) = passes.posterize_op {
        blend_posterize(pixels, entries, passes, opacity, passes.posterize_num);
    }
    true
}

/// Render one dab row (fixed `y` within the tile).
#[inline]
fn draw_dab_row(
    pixels: &mut [[u16; 4]],
    xl: f32,
    yl: f32,
    yp: f32,
    geom: &DabGeometry,
    passes: &DabPasses,
) -> bool {
    let mut entries = [(0usize, 0u32); TILE_SIZE];
    let n = mask_row_into(&mut entries, 0, xl, yl, yp, geom);
    if n == 0 {
        false
    } else {
        blend_entries(pixels, &entries[..n], passes)
    }
}

thread_local! {
    /// 每个线程复用的 tile 级条目缓冲，避免每个 dab×tile 都分配。
    static ENTRY_SCRATCH: std::cell::RefCell<Vec<(usize, u32)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Process the tile rows `y0..=y1` of one tile.
fn draw_tile_rows(
    tile: &mut TilePixels,
    xl: f32,
    yl: f32,
    y0: usize,
    y1: usize,
    geom: &DabGeometry,
    passes: &DabPasses,
) -> bool {
    let mut any = false;
    ENTRY_SCRATCH.with(|cell| {
        let mut entries = cell.borrow_mut();
        entries.clear();
        let mut row_entries = [(0usize, 0u32); TILE_SIZE];
        for row in y0..=y1 {
            let n = mask_row_into(&mut row_entries, row * TILE_SIZE, xl, yl, row as f32, geom);
            if n > 0 {
                entries.extend_from_slice(&row_entries[..n]);
                any = true;
            }
        }
        if any {
            blend_entries(tile.as_flattened_mut(), &entries, passes);
        }
    });
    any
}

/// Render `dab` into `surface`. Returns whether any pixel was modified.
///
/// This is the function `TiledSurface::draw_dab` defaults to.
/// 预先算好的 dab 几何 / 混合参数，可跨线程共享。
pub struct PreparedDab {
    x: f32,
    y: f32,
    tx0: i32,
    ty0: i32,
    tx1: i32,
    ty1: i32,
    geom: DabGeometry,
    passes: DabPasses,
}

impl PreparedDab {
    /// 该 dab 覆盖的 tile 坐标（含两端）。
    #[inline]
    pub fn tiles(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        (self.ty0..=self.ty1).flat_map(move |ty| (self.tx0..=self.tx1).map(move |tx| (tx, ty)))
    }

    #[inline]
    pub fn tile_count(&self) -> usize {
        ((self.tx1 - self.tx0 + 1).max(0) * (self.ty1 - self.ty0 + 1).max(0)) as usize
    }

    /// 椭圆的大致像素面积（用于选择并行粒度）。
    #[inline]
    pub fn estimated_pixels(&self) -> f32 {
        core::f32::consts::PI / self.geom.inv_r2 / self.geom.aspect
    }

    /// 该 tile 内需要处理的行范围（含两端）；与椭圆无交时返回 `None`。
    #[inline]
    pub fn row_range(&self, tx: i32, ty: i32) -> Option<(usize, usize)> {
        let ts = TILE_SIZE as i32;
        let xl = self.x - (tx * ts) as f32;
        let yl = self.y - (ty * ts) as f32;
        // AA coverage can reach outside the ellipse, so sub-3px dabs use
        // libmypaint's square bbox for the row range too.
        let y_extent = if self.geom.r_aa_start >= 0.0 {
            self.geom.r_fringe
        } else {
            self.geom.hy_fringe
        };
        let ry0 = ((yl - y_extent).floor() as i32).max(0);
        let ry1 = ((yl + y_extent).floor() as i32).min(ts - 1);
        if ry0 > ry1 || xl + self.geom.r_fringe < 0.0 || xl - self.geom.r_fringe > ts as f32 {
            return None;
        }
        Some((ry0 as usize, ry1 as usize))
    }
}

/// 一次性传给 GPU 光栅器的 dab 参数（纯标量，无借用）。
///
/// 只有 [`PreparedDab::gpu_params`] 能完整表达、且与 CPU 光栅器逐位一致的
/// dab 才会产出这个结构；其余 dab 仍走 CPU。当前支持：
/// `paint == 1`、任意 `color_a`（含部分擦除）、无 lock_alpha / colorize /
/// posterize、半径 ≥ 3（无抗锯齿子采样）。
#[derive(Clone, Copy, Debug)]
pub struct GpuDabParams {
    pub x: f32,
    pub y: f32,
    pub r_fringe: f32,
    pub aspect: f32,
    pub cs: f32,
    pub sn: f32,
    pub inv_r2: f32,
    pub hardness: f32,
    pub softness: f32,
    pub color_a: u32,
    pub src_r: u32,
    pub src_g: u32,
    pub src_b: u32,
    pub opacity: u32,
    pub spec_a_log: [f32; 10],
    pub spec_a_zero_pow: [f32; 10],
}

impl PreparedDab {
    /// GPU 光栅器参数；返回 `None` 表示该 dab 必须走 CPU。
    pub fn gpu_params(&self) -> Option<GpuDabParams> {
        let p = &self.passes;
        let paint_only = p.normal_legacy.is_none()
            && p.lock_alpha_legacy.is_none()
            && p.normal_paint.is_some()
            && p.lock_alpha_paint.is_none()
            && p.colorize_op.is_none()
            && p.posterize_op.is_none();
        if !paint_only || self.geom.r_aa_start >= 0.0 {
            return None;
        }
        Some(GpuDabParams {
            x: self.x,
            y: self.y,
            r_fringe: self.geom.r_fringe,
            aspect: self.geom.aspect,
            cs: self.geom.cs,
            sn: self.geom.sn,
            inv_r2: self.geom.inv_r2,
            hardness: self.geom.hardness,
            softness: self.geom.softness,
            color_a: p.color_a,
            src_r: p.src_r,
            src_g: p.src_g,
            src_b: p.src_b,
            opacity: p.normal_paint.unwrap_or(0),
            spec_a_log: p.spec_a_log,
            spec_a_zero_pow: p.spec_a_zero_pow,
        })
    }
}

/// 计算 dab 的 mask/混合参数与 tile 范围；退化 dab 返回 `None`。
pub fn prepare_dab(dab: &Dab) -> Option<PreparedDab> {
    // libmypaint's draw_dab_internal rejects degenerate dabs outright —
    // mirror that so low-pressure ramps don't lay down a sub-pixel "smear"
    // hokusai used to render as a linear falloff. Without the hardness ≤ 0
    // check, AA pushes the brush into a regime libmypaint treats as a
    // no-op.
    let softness = dab.softness.clamp(0.0, 1.0);
    if dab.radius < 0.1 || dab.hardness <= 0.0 || dab.opaque <= 0.0 || softness >= 1.0 {
        return None;
    }
    if trace_dabs() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        eprintln!(
            "  hokbits: {:08x} {:08x} {:08x} {:08x}",
            dab.x.to_bits(), dab.y.to_bits(), dab.radius.to_bits(), dab.hardness.to_bits()
        );
        eprintln!(
            "  hok#{}: ({:9.4},{:9.4}) r={:8.5} hard={:7.5} opaq={:7.5} aspect={:7.4} ang={:8.3} paint={:4.2} rgb=({:8.5},{:8.5},{:8.5}) er={:7.5}",
            n, dab.x, dab.y, dab.radius, dab.hardness, dab.opaque,
            dab.aspect_ratio, dab.angle, dab.paint,
            dab.color.r, dab.color.g, dab.color.b, dab.alpha_eraser,
        );
    }
    // libmypaint rejects radius < 0.1 above and otherwise renders with
    // the actual radius (mypaint-tiled-surface.c:575). hokusai used to
    // floor at 0.5 here, which made very thin pencils render thicker.
    let radius = dab.radius;
    let aspect = dab.aspect_ratio.max(1.0);
    // C render_dab_mask: `float angle_rad = angle/360*2*M_PI` — f32 up to
    // the `*2`, then promoted to double for the M_PI multiply and narrowed
    // back. Keep the same rounding points.
    let angle = ((dab.angle / 360.0 * 2.0) as f64 * std::f64::consts::PI) as f32;
    // C: `float cs = cos(angle_rad)` — double trig narrowed to float.
    let cs = (angle as f64).cos() as f32;
    let sn = (angle as f64).sin() as f32;
    // libmypaint's render_dab_mask uses per-pixel sub-pixel sampling
    // (calculate_rr_antialiased) for `radius < 3`. Larger dabs fall back
    // to the plain rr formula since aliasing is invisible. Pre-compute
    // `r_aa_start` here so the render functions can branch on a single
    // scalar instead of re-deriving it per pixel.
    let r_aa_start = if radius < 3.0 {
        let aa_border = 1.0_f32;
        let start = if radius > aa_border {
            radius - aa_border
        } else {
            0.0
        };
        start * start / aspect
    } else {
        -1.0
    };
    // anti_aliasing was baked into the (radius, hardness) pair earlier
    // in stroke.rs::build_dab — same trick libmypaint uses in
    // prepare_and_draw_dab. dab.anti_aliasing is unused on the render
    // side; the per-pixel sub-pixel sampling for small dabs is driven by
    // the libmypaint-correct `r_aa_start` path computed above.
    let _ = dab.anti_aliasing;

    // 椭圆 y 半轴（含 fringe）。行的有效范围由它决定；`hx`/`hy` 上面的
    // tile 范围也复用同一套几何。
    let hy = radius * (sn * sn + (cs / aspect) * (cs / aspect)).sqrt();
    let geom = DabGeometry {
        r_fringe: radius + 1.0,
        hy_fringe: (radius + 1.0) * (sn * sn + (cs / aspect) * (cs / aspect)).sqrt(),
        aspect,
        cs,
        sn,
        inv_r2: 1.0 / (radius * radius),
        r_aa_start,
        hardness: dab.hardness.clamp(0.0, 1.0),
        softness,
    };
    let passes = DabPasses::new(dab);

    // Tight axis-aligned bounds of the rotated ellipse. libmypaint walks the
    // square `radius + 1` box for every dab; for elongated brushes (knives,
    // aspect 5-6.5) that spends most of its mask time outside the ellipse.
    // The extent of the quadratic form along x/y is
    //   hx = r·√(cs² + (sn/aspect)²),  hy = r·√(sn² + (cs/aspect)²).
    // The extra 1 px is the same safety fringe the old square box used.
    let hx = radius * (cs * cs + (sn / aspect) * (sn / aspect)).sqrt();
    let x0 = (dab.x - hx - 1.0).floor() as i32;
    let y0 = (dab.y - hy - 1.0).floor() as i32;
    let x1 = (dab.x + hx + 1.0).floor() as i32;
    let y1 = (dab.y + hy + 1.0).floor() as i32;
    let ts = TILE_SIZE as i32;
    let tx0 = x0.div_euclid(ts);
    let ty0 = y0.div_euclid(ts);
    let tx1 = x1.div_euclid(ts);
    let ty1 = y1.div_euclid(ts);
    if tx0 > tx1 || ty0 > ty1 {
        return None;
    }
    Some(PreparedDab {
        x: dab.x,
        y: dab.y,
        tx0,
        ty0,
        tx1,
        ty1,
        geom,
        passes,
    })
}

/// 在一个 tile 上执行已准备好的 dab；多个 tile 互不重叠，可并行。
pub fn draw_prepared_dab_tile(
    tile: &mut TilePixels,
    tx: i32,
    ty: i32,
    prepared: &PreparedDab,
) -> bool {
    let Some((ry0, ry1)) = prepared.row_range(tx, ty) else {
        return false;
    };
    let ts = TILE_SIZE as i32;
    let xl = prepared.x - (tx * ts) as f32;
    let yl = prepared.y - (ty * ts) as f32;
    draw_tile_rows(tile, xl, yl, ry0, ry1, &prepared.geom, &prepared.passes)
}

/// 在一个 tile 的单行上执行已准备好的 dab。
///
/// 行与行之间没有共享状态，这是比「按 tile 并行」更细的并行单位：
/// 低缩放下一个刮刀 dab 覆盖 6–8 个 tile、约两百行，按行切能把工作
/// 平均分给所有核心，而按 tile 切只有 6–8 个任务。
pub fn draw_prepared_dab_row(
    tile: &mut TilePixels,
    tx: i32,
    ty: i32,
    row: usize,
    prepared: &PreparedDab,
) -> bool {
    let ts = TILE_SIZE as i32;
    let xl = prepared.x - (tx * ts) as f32;
    let yl = prepared.y - (ty * ts) as f32;
    draw_dab_row(
        &mut tile[row],
        xl,
        yl,
        row as f32,
        &prepared.geom,
        &prepared.passes,
    )
}

/// 在任意 `TiledSurface` 上顺序绘制已准备好的 dab（参考实现）。
pub fn draw_prepared_dab<S: TiledSurface + ?Sized>(
    surface: &mut S,
    prepared: &PreparedDab,
) -> bool {
    let mut painted = false;
    for (tx, ty) in prepared.tiles() {
        let tile = surface.tile_request_start(tx, ty);
        painted |= draw_prepared_dab_tile(tile, tx, ty, prepared);
        surface.tile_request_end(tx, ty);
    }
    painted
}

pub fn draw_dab_default<S: TiledSurface + ?Sized>(surface: &mut S, dab: &Dab) -> bool {
    let Some(prepared) = prepare_dab(dab) else {
        return false;
    };
    draw_prepared_dab(surface, &prepared)
}

/// `draw_dab_pixels_BlendMode_Normal` — premultiplied source-over in fix15
/// integers, libmypaint's exact truncation points.
fn blend_normal(
    pixels: &mut [[u16; 4]],
    entries: &[(usize, u32)],
    passes: &DabPasses,
    opacity: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, mask) in entries {
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let p = &mut pixels[lx];
        p[3] = (opa_a + opa_b * p[3] as u32 / ONE) as u16;
        p[0] = ((opa_a * passes.src_r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * passes.src_g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * passes.src_b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_Normal_and_Eraser` — the erase-aware variant
/// (smudging dabs): the source contribution scales with `color_a` while the
/// destination keeps the un-scaled coverage, so `color_a < 1` erases.
fn blend_normal_and_eraser(
    pixels: &mut [[u16; 4]],
    entries: &[(usize, u32)],
    passes: &DabPasses,
    color_a: u32,
    opacity: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, mask) in entries {
        let mut opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        opa_a = opa_a * color_a / ONE;
        let p = &mut pixels[lx];
        p[3] = (opa_a + opa_b * p[3] as u32 / ONE) as u16;
        p[0] = ((opa_a * passes.src_r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * passes.src_g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * passes.src_b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_LockAlpha` — colour-only blend masked by the
/// destination alpha; dst alpha is left untouched.
fn blend_lock_alpha(
    pixels: &mut [[u16; 4]],
    entries: &[(usize, u32)],
    passes: &DabPasses,
    opacity: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, mask) in entries {
        let mut opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let p = &mut pixels[lx];
        opa_a *= p[3] as u32;
        opa_a /= ONE;
        p[0] = ((opa_a * passes.src_r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * passes.src_g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * passes.src_b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_LockAlpha_Paint` — the spectral flavour of
/// lock-alpha (min opacity 150 like Normal_Paint).
fn blend_lock_alpha_paint(
    pixels: &mut [[u16; 4]],
    entries: &[(usize, u32)],
    passes: &DabPasses,
    opacity: u32,
) {
    use crate::spectral::{fastpow, fastpow_from_log, rgb_to_spectral, spectral_to_rgb};
    const ONE: u32 = FIX15_ONE;
    let opacity = opacity.max(150);
    for &(lx, mask) in entries {
        let mut opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let p = &mut pixels[lx];
        opa_a *= p[3] as u32;
        opa_a /= ONE;
        let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
        if a0 == 0 {
            p[0] = ((opa_a * passes.src_r + opa_b * r0) / ONE) as u16;
            p[1] = ((opa_a * passes.src_g + opa_b * g0) / ONE) as u16;
            p[2] = ((opa_a * passes.src_b + opa_b * b0) / ONE) as u16;
            continue;
        }
        let fac_a = opa_a as f32 / (opa_a + opa_b * a0 / ONE) as f32;
        let fac_b = 1.0 - fac_a;
        let spec_b = rgb_to_spectral(
            r0 as f32 / a0 as f32,
            g0 as f32 / a0 as f32,
            b0 as f32 / a0 as f32,
        );
        let mut mix = [0.0_f32; 10];
        for i in 0..10 {
            mix[i] = fastpow_from_log(passes.spec_a_log[i], fac_a) * fastpow(spec_b[i], fac_b);
        }
        let (sr, sg, sb) = spectral_to_rgb(&mix);
        // LockAlpha_Paint writes colour only: the destination alpha is the
        // lock. libmypaint re-premultiplies by the original `rgba[3]` and
        // never assigns to it (brushmodes.c:481-483).
        p[0] = (sr * a0 as f32 + 0.5) as u16;
        p[1] = (sg * a0 as f32 + 0.5) as u16;
        p[2] = (sb * a0 as f32 + 0.5) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_Color` — Adobe "Color" non-separable blend:
/// transfer the destination's BT.601 luminance onto the source hue/chroma,
/// in the same scaled-int arithmetic (LUMA coefficients are floats, the
/// clip steps are integer divisions).
fn blend_color(
    pixels: &mut [[u16; 4]],
    entries: &[(usize, u32)],
    passes: &DabPasses,
    opacity: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, mask) in entries {
        let p = &mut pixels[lx];
        let a = p[3] as u32;
        // De-premult — `checked_div` matches C's "leave r=g=b=0 when
        // alpha is 0" fallback exactly.
        let mut r = (ONE * p[0] as u32).checked_div(a).unwrap_or(0) as u16;
        let mut g = (ONE * p[1] as u32).checked_div(a).unwrap_or(0) as u16;
        let mut b = (ONE * p[2] as u32).checked_div(a).unwrap_or(0) as u16;
        set_rgb16_lum_from_rgb16(
            passes.src_r as u16,
            passes.src_g as u16,
            passes.src_b as u16,
            &mut r,
            &mut g,
            &mut b,
        );
        // Re-premult.
        let r = r as u32 * a / ONE;
        let g = g as u32 * a / ONE;
        let b = b as u32 * a / ONE;
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        p[0] = ((opa_a * r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// BT.601 luminance of a fix15 triple, as the C `LUMA` macro: float
/// coefficients pre-scaled by 2^15, so the product needs a /2^15 after.
#[inline]
fn luma(r: f32, g: f32, b: f32) -> f32 {
    const LUMA_RED_COEFF: f32 = 0.2126 * (1 << 15) as f32;
    const LUMA_GREEN_COEFF: f32 = 0.7152 * (1 << 15) as f32;
    const LUMA_BLUE_COEFF: f32 = 0.0722 * (1 << 15) as f32;
    r * LUMA_RED_COEFF + g * LUMA_GREEN_COEFF + b * LUMA_BLUE_COEFF
}

/// Port of brushmodes.c `set_rgb16_lum_from_rgb16` (Adobe SetLum +
/// ClipColor in scaled ints).
fn set_rgb16_lum_from_rgb16(
    topr: u16,
    topg: u16,
    topb: u16,
    botr: &mut u16,
    botg: &mut u16,
    botb: &mut u16,
) {
    const ONE: i32 = FIX15_ONE as i32;
    let botlum = (luma(*botr as f32, *botg as f32, *botb as f32) / ONE as f32) as u16;
    let toplum = (luma(topr as f32, topg as f32, topb as f32) / ONE as f32) as u16;
    let diff = botlum as i32 - toplum as i32;
    let mut r = topr as i32 + diff;
    let mut g = topg as i32 + diff;
    let mut b = topb as i32 + diff;

    let lum = (luma(r as f32, g as f32, b as f32) / ONE as f32) as i32;
    let cmin = r.min(g).min(b);
    let cmax = r.max(g).max(b);
    if cmin < 0 {
        r = lum + ((r - lum) * lum) / (lum - cmin);
        g = lum + ((g - lum) * lum) / (lum - cmin);
        b = lum + ((b - lum) * lum) / (lum - cmin);
    }
    if cmax > ONE {
        r = lum + ((r - lum) * (ONE - lum)) / (cmax - lum);
        g = lum + ((g - lum) * (ONE - lum)) / (cmax - lum);
        b = lum + ((b - lum) * (ONE - lum)) / (cmax - lum);
    }
    *botr = r as u16;
    *botg = g as u16;
    *botb = b as u16;
}

/// `draw_dab_pixels_BlendMode_Posterize` — quantise the (premultiplied)
/// canvas under the dab toward `posterize_num` levels.
fn blend_posterize(
    pixels: &mut [[u16; 4]],
    entries: &[(usize, u32)],
    _passes: &DabPasses,
    opacity: u32,
    posterize_num: u32,
) {
    const ONE: u32 = FIX15_ONE;
    for &(lx, mask) in entries {
        let p = &mut pixels[lx];
        let r = p[0] as f32 / ONE as f32;
        let g = p[1] as f32 / ONE as f32;
        let b = p[2] as f32 / ONE as f32;
        // C: `(1<<15) * ROUND(r * posterize_num) / posterize_num` with
        // ROUND(x) = floor(x + 0.5) and integer division.
        let post_r = ONE * ((r * posterize_num as f32 + 0.5).floor() as u32) / posterize_num;
        let post_g = ONE * ((g * posterize_num as f32 + 0.5).floor() as u32) / posterize_num;
        let post_b = ONE * ((b * posterize_num as f32 + 0.5).floor() as u32) / posterize_num;
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        p[0] = ((opa_a * post_r + opa_b * p[0] as u32) / ONE) as u16;
        p[1] = ((opa_a * post_g + opa_b * p[1] as u32) / ONE) as u16;
        p[2] = ((opa_a * post_b + opa_b * p[2] as u32) / ONE) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_Normal_Paint` — full WGM spectral
/// source-over for non-smudging paint dabs (min opacity 150/2^15).
///
/// The WGM evaluation is batched 8 pixels at a time on AVX2 CPUs. Lanes with
/// a fully transparent destination use libmypaint's additive fast path and are
/// written scalar afterwards, exactly like the reference loop.
fn blend_normal_paint(
    pixels: &mut [[u16; 4]],
    entries: &[(usize, u32)],
    passes: &DabPasses,
    opacity: u32,
) {
    use crate::spectral::{fastpow, fastpow_from_log, rgb_to_spectral, spectral_to_rgb};
    const ONE: u32 = FIX15_ONE;
    // "pigment-mode does not like very low opacity" — brushmodes.c:87.
    let opacity = opacity.max(150);
    let mut idx = 0usize;
    #[cfg(target_arch = "x86_64")]
    if passes.simd && entries.len() >= 8 {
        while idx + 8 <= entries.len() {
            let batch = &entries[idx..idx + 8];
            let mut fac_a = [0.0f32; 8];
            let mut fac_b = [0.0f32; 8];
            let mut spec_b = [[0.0f32; 8]; 10];
            let mut a_out_lane = [0u32; 8];
            let mut a0_lane = [0u32; 8];
            let mut any_spectral = false;
            for (lane, &(lx, mask)) in batch.iter().enumerate() {
                let opa_a = mask * opacity / ONE;
                let opa_b = ONE - opa_a;
                let p = &pixels[lx];
                let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
                let a_out = opa_a + opa_b * a0 / ONE;
                a_out_lane[lane] = a_out;
                a0_lane[lane] = a0;
                if a0 == 0 {
                    continue;
                }
                any_spectral = true;
                let fa = opa_a as f32 / a_out as f32;
                fac_a[lane] = fa;
                fac_b[lane] = 1.0 - fa;
                let spec = rgb_to_spectral(
                    r0 as f32 / a0 as f32,
                    g0 as f32 / a0 as f32,
                    b0 as f32 / a0 as f32,
                );
                for i in 0..10 {
                    spec_b[i][lane] = spec[i];
                }
            }
            // 全透明目标走加法快路径，WGM 结果无意义；整批跳过向量
            // WGM 可以把空白画布上的大笔刷从 0.5µs/px 拉回 ~30ns/px。
            let (sr, sg, sb) = if any_spectral {
                unsafe {
                    crate::spectral::mix_spectral8(&passes.spec_a_log, &fac_a, &fac_b, &spec_b)
                }
            } else {
                ([0.0f32; 8], [0.0f32; 8], [0.0f32; 8])
            };
            for (lane, &(lx, mask)) in batch.iter().enumerate() {
                let p = &mut pixels[lx];
                if a0_lane[lane] == 0 {
                    let opa_a = mask * opacity / ONE;
                    let opa_b = ONE - opa_a;
                    let a_out = a_out_lane[lane];
                    p[3] = a_out as u16;
                    p[0] = ((opa_a * passes.src_r + opa_b * p[0] as u32) / ONE) as u16;
                    p[1] = ((opa_a * passes.src_g + opa_b * p[1] as u32) / ONE) as u16;
                    p[2] = ((opa_a * passes.src_b + opa_b * p[2] as u32) / ONE) as u16;
                    continue;
                }
                let a_out = a_out_lane[lane];
                p[3] = a_out as u16;
                p[0] = (sr[lane] * a_out as f32 + 0.5) as u16;
                p[1] = (sg[lane] * a_out as f32 + 0.5) as u16;
                p[2] = (sb[lane] * a_out as f32 + 0.5) as u16;
            }
            idx += 8;
        }
    }
    for &(lx, mask) in &entries[idx..] {
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let p = &mut pixels[lx];
        let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
        let a_out = opa_a + opa_b * a0 / ONE;
        if a0 == 0 {
            // Nothing to mix with — plain additive.
            p[3] = a_out as u16;
            p[0] = ((opa_a * passes.src_r + opa_b * r0) / ONE) as u16;
            p[1] = ((opa_a * passes.src_g + opa_b * g0) / ONE) as u16;
            p[2] = ((opa_a * passes.src_b + opa_b * b0) / ONE) as u16;
            continue;
        }
        let fac_a = opa_a as f32 / (opa_a + opa_b * a0 / ONE) as f32;
        let fac_b = 1.0 - fac_a;
        let spec_b = rgb_to_spectral(
            r0 as f32 / a0 as f32,
            g0 as f32 / a0 as f32,
            b0 as f32 / a0 as f32,
        );
        let mut mix = [0.0_f32; 10];
        for i in 0..10 {
            mix[i] = fastpow_from_log(passes.spec_a_log[i], fac_a) * fastpow(spec_b[i], fac_b);
        }
        let (sr, sg, sb) = spectral_to_rgb(&mix);
        p[3] = a_out as u16;
        // C: `rgba[i] = (rgb_result[i] * rgba[3]) + 0.5;`
        p[0] = (sr * a_out as f32 + 0.5) as u16;
        p[1] = (sg * a_out as f32 + 0.5) as u16;
        p[2] = (sb * a_out as f32 + 0.5) as u16;
    }
}

/// `draw_dab_pixels_BlendMode_Normal_and_Eraser_Paint` 在 `color_a == 0`
/// 时的精确快捷路径。
///
/// 源色权重为 0，于是 `fac_a` 恒为 0、`fac_b` 恒为 1：WGM 只剩
/// `spec_b^1` 一侧。`fastpow(spec_a, 0)` 是每个 dab 的常量，预先算好；
/// 每个像素少一次除法、十次 `fastpow`，输出与通用路径逐位相同。
fn blend_eraser_only_paint(
    pixels: &mut [[u16; 4]],
    entries: &[(usize, u32)],
    passes: &DabPasses,
    opacity: u32,
) {
    use crate::spectral::{fastpow, rgb_to_spectral, spectral_blend_factor, spectral_to_rgb};
    const ONE: u32 = FIX15_ONE;
    let mut idx = 0usize;
    #[cfg(target_arch = "x86_64")]
    if passes.simd && entries.len() >= 8 {
        while idx + 8 <= entries.len() {
            let batch = &entries[idx..idx + 8];
            let mut spec_b = [[0.0f32; 8]; 10];
            let mut additive = [[0u32; 8]; 3];
            let mut spectral_factor = [0.0f32; 8];
            let mut additive_factor = [0.0f32; 8];
            let mut opa_out_lane = [0u32; 8];
            let mut use_spectral = [false; 8];
            for (lane, &(lx, mask)) in batch.iter().enumerate() {
                let opa_a = mask * opacity / ONE;
                let opa_b = ONE - opa_a;
                let p = &pixels[lx];
                let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
                let opa_out = opa_b * a0 / ONE;
                opa_out_lane[lane] = opa_out;
                let sf = spectral_blend_factor(a0 as f32 / ONE as f32).clamp(0.0, 1.0);
                let af = 1.0 - sf;
                spectral_factor[lane] = sf;
                additive_factor[lane] = af;
                if af != 0.0 {
                    additive[0][lane] = opa_b * r0 / ONE;
                    additive[1][lane] = opa_b * g0 / ONE;
                    additive[2][lane] = opa_b * b0 / ONE;
                }
                if sf != 0.0 && a0 != 0 {
                    use_spectral[lane] = true;
                    let spec = rgb_to_spectral(
                        r0 as f32 / a0 as f32,
                        g0 as f32 / a0 as f32,
                        b0 as f32 / a0 as f32,
                    );
                    for i in 0..10 {
                        spec_b[i][lane] = spec[i];
                    }
                }
            }
            let any_spectral = use_spectral.iter().any(|&v| v);
            let (sr, sg, sb) = if any_spectral {
                unsafe {
                    crate::spectral::mix_spectral8_single(&passes.spec_a_zero_pow, &spec_b)
                }
            } else {
                ([0.0f32; 8], [0.0f32; 8], [0.0f32; 8])
            };
            for (lane, &(lx, _mask)) in batch.iter().enumerate() {
                let p = &mut pixels[lx];
                let mut rgb = [additive[0][lane], additive[1][lane], additive[2][lane]];
                if use_spectral[lane] {
                    let sf = spectral_factor[lane];
                    let af = additive_factor[lane];
                    let opa_out = opa_out_lane[lane];
                    let res = [sr[lane], sg[lane], sb[lane]];
                    for i in 0..3 {
                        rgb[i] = (af * rgb[i] as f32 + sf * res[i] * opa_out as f32) as u32;
                    }
                }
                p[3] = opa_out_lane[lane] as u16;
                p[0] = rgb[0] as u16;
                p[1] = rgb[1] as u16;
                p[2] = rgb[2] as u16;
            }
            idx += 8;
        }
    }
    for &(lx, mask) in &entries[idx..] {
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let p = &mut pixels[lx];
        let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
        let opa_out = opa_b * a0 / ONE;

        let mut rgb = [0u32; 3];
        let spectral_factor = spectral_blend_factor(a0 as f32 / ONE as f32).clamp(0.0, 1.0);
        let additive_factor = 1.0 - spectral_factor;
        if additive_factor != 0.0 {
            rgb[0] = opa_b * r0 / ONE;
            rgb[1] = opa_b * g0 / ONE;
            rgb[2] = opa_b * b0 / ONE;
        }
        if spectral_factor != 0.0 && a0 != 0 {
            let spec_b = rgb_to_spectral(
                r0 as f32 / a0 as f32,
                g0 as f32 / a0 as f32,
                b0 as f32 / a0 as f32,
            );
            let mut mix = [0.0_f32; 10];
            for i in 0..10 {
                mix[i] = passes.spec_a_zero_pow[i] * fastpow(spec_b[i], 1.0);
            }
            let (sr, sg, sb) = spectral_to_rgb(&mix);
            let res = [sr, sg, sb];
            for i in 0..3 {
                rgb[i] = (additive_factor * rgb[i] as f32
                    + spectral_factor * res[i] * opa_out as f32) as u32;
            }
        }

        p[3] = opa_out as u16;
        p[0] = rgb[0] as u16;
        p[1] = rgb[1] as u16;
        p[2] = rgb[2] as u16;
    }
}

/// `draw_dab_pixels_BlendMode_Normal_and_Eraser_Paint` — erase-aware
/// spectral blend that fades from additive to WGM with
/// `spectral_blend_factor(canvas alpha)`; no minimum opacity.
fn blend_normal_and_eraser_paint(
    pixels: &mut [[u16; 4]],
    entries: &[(usize, u32)],
    passes: &DabPasses,
    color_a: u32,
    opacity: u32,
) {
    use crate::spectral::{
        fastpow, fastpow_from_log, rgb_to_spectral, spectral_blend_factor, spectral_to_rgb,
    };
    const ONE: u32 = FIX15_ONE;
    let mut idx = 0usize;
    // color_a == 0 时向量批次只增加每 lane 的簿记开销，标量更快；
    // 部分擦除（水彩/涂抹笔刷，0 < color_a < 1）则能从中获益。
    #[cfg(target_arch = "x86_64")]
    if passes.simd && color_a != 0 && entries.len() >= 8 {
        while idx + 8 <= entries.len() {
            let batch = &entries[idx..idx + 8];
            let mut fac_a = [0.0f32; 8];
            let mut fac_b = [0.0f32; 8];
            let mut spec_b = [[0.0f32; 8]; 10];
            let mut additive = [[0u32; 8]; 3];
            let mut spectral_factor = [0.0f32; 8];
            let mut additive_factor = [0.0f32; 8];
            let mut opa_out_lane = [0u32; 8];
            let mut use_spectral = [false; 8];
            for (lane, &(lx, mask)) in batch.iter().enumerate() {
                let opa_a = mask * opacity / ONE;
                let opa_b = ONE - opa_a;
                let opa_a2 = opa_a * color_a / ONE;
                let p = &pixels[lx];
                let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
                let opa_out = opa_a2 + opa_b * a0 / ONE;
                opa_out_lane[lane] = opa_out;
                let sf = spectral_blend_factor(a0 as f32 / ONE as f32).clamp(0.0, 1.0);
                let af = 1.0 - sf;
                spectral_factor[lane] = sf;
                additive_factor[lane] = af;
                if af != 0.0 {
                    additive[0][lane] = (opa_a2 * passes.src_r + opa_b * r0) / ONE;
                    additive[1][lane] = (opa_a2 * passes.src_g + opa_b * g0) / ONE;
                    additive[2][lane] = (opa_a2 * passes.src_b + opa_b * b0) / ONE;
                }
                if sf != 0.0 && a0 != 0 {
                    use_spectral[lane] = true;
                    let spec = rgb_to_spectral(
                        r0 as f32 / a0 as f32,
                        g0 as f32 / a0 as f32,
                        b0 as f32 / a0 as f32,
                    );
                    for i in 0..10 {
                        spec_b[i][lane] = spec[i];
                    }
                    let mut fa = opa_a as f32 / (opa_a + opa_b * a0 / ONE) as f32;
                    fa *= color_a as f32 / ONE as f32;
                    fac_a[lane] = fa;
                    fac_b[lane] = 1.0 - fa;
                }
            }
            let any_spectral = use_spectral.iter().any(|&v| v);
            let (sr, sg, sb) = if any_spectral {
                unsafe {
                    crate::spectral::mix_spectral8(&passes.spec_a_log, &fac_a, &fac_b, &spec_b)
                }
            } else {
                ([0.0f32; 8], [0.0f32; 8], [0.0f32; 8])
            };
            for (lane, &(lx, _mask)) in batch.iter().enumerate() {
                let p = &mut pixels[lx];
                let mut rgb = [additive[0][lane], additive[1][lane], additive[2][lane]];
                if use_spectral[lane] {
                    let sf = spectral_factor[lane];
                    let af = additive_factor[lane];
                    let opa_out = opa_out_lane[lane];
                    let res = [sr[lane], sg[lane], sb[lane]];
                    for i in 0..3 {
                        // uint32 ← float expression: truncation, like C.
                        rgb[i] = (af * rgb[i] as f32 + sf * res[i] * opa_out as f32) as u32;
                    }
                }
                p[3] = opa_out_lane[lane] as u16;
                p[0] = rgb[0] as u16;
                p[1] = rgb[1] as u16;
                p[2] = rgb[2] as u16;
            }
            idx += 8;
        }
    }
    for &(lx, mask) in &entries[idx..] {
        let opa_a = mask * opacity / ONE;
        let opa_b = ONE - opa_a;
        let opa_a2 = opa_a * color_a / ONE;
        let p = &mut pixels[lx];
        let (r0, g0, b0, a0) = (p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32);
        let opa_out = opa_a2 + opa_b * a0 / ONE;

        let mut rgb = [0u32; 3];
        let spectral_factor = spectral_blend_factor(a0 as f32 / ONE as f32).clamp(0.0, 1.0);
        let additive_factor = 1.0 - spectral_factor;

        if additive_factor != 0.0 {
            rgb[0] = (opa_a2 * passes.src_r + opa_b * r0) / ONE;
            rgb[1] = (opa_a2 * passes.src_g + opa_b * g0) / ONE;
            rgb[2] = (opa_a2 * passes.src_b + opa_b * b0) / ONE;
        }

        if spectral_factor != 0.0 && a0 != 0 {
            let spec_b = rgb_to_spectral(
                r0 as f32 / a0 as f32,
                g0 as f32 / a0 as f32,
                b0 as f32 / a0 as f32,
            );
            let mut fac_a = opa_a as f32 / (opa_a + opa_b * a0 / ONE) as f32;
            fac_a *= color_a as f32 / ONE as f32;
            let fac_b = 1.0 - fac_a;
            let mut mix = [0.0_f32; 10];
            for i in 0..10 {
                mix[i] = fastpow_from_log(passes.spec_a_log[i], fac_a) * fastpow(spec_b[i], fac_b);
            }
            let (sr, sg, sb) = spectral_to_rgb(&mix);
            let res = [sr, sg, sb];
            for i in 0..3 {
                // uint32 ← float expression: truncation, like C.
                rgb[i] = (additive_factor * rgb[i] as f32
                    + spectral_factor * res[i] * opa_out as f32) as u32;
            }
        }

        p[3] = opa_out as u16;
        p[0] = rgb[0] as u16;
        p[1] = rgb[1] as u16;
        p[2] = rgb[2] as u16;
    }
}
#[inline]
fn clamp_color(c: RgbaF32) -> RgbaF32 {
    RgbaF32 {
        r: c.r.clamp(0.0, 1.0),
        g: c.g.clamp(0.0, 1.0),
        b: c.b.clamp(0.0, 1.0),
        a: c.a.clamp(0.0, 1.0),
    }
}

/// Backend-agnostic version of [`get_color_default`]: takes a
/// `sample(px, py)` closure that returns a single fix15 RGBA pixel
/// (`[r, g, b, a]`, premultiplied, linear sRGB). Backends that can't
/// hand out raw `TilePixels` (e.g. a `Pixmap`-only surface) can
/// override `TiledSurface::get_color` to delegate here, passing a
/// closure that reads from their own buffer.
///
/// Coordinates outside the painted area should be reported as fully
/// transparent (`[0, 0, 0, 0]`).
pub fn get_color_via_sample<F>(x: f32, y: f32, radius: f32, sample: F) -> RgbaF32
where
    F: Fn(i32, i32) -> [u16; 4],
{
    let radius = radius.max(1.0); // libmypaint floors get_color radius at 1.0 (mypaint-tiled-surface.c:659)
    let inv_r2 = 1.0 / (radius * radius);
    let r_ext = radius + 1.0;
    let x0 = (x - r_ext).floor() as i32;
    let y0 = (y - r_ext).floor() as i32;
    let x1 = (x + r_ext).ceil() as i32;
    let y1 = (y + r_ext).ceil() as i32;

    let mut sum_r = 0.0f32;
    let mut sum_g = 0.0f32;
    let mut sum_b = 0.0f32;
    let mut sum_a = 0.0f32;
    let mut sum_w = 0.0f32;

    for py in y0..=y1 {
        for px in x0..=x1 {
            let rr = rr_at(px as f32, py as f32, x, y, 1.0, 1.0, 0.0, inv_r2);
            if rr > 1.0 {
                continue;
            }
            // libmypaint's get_color() hardcodes softness = 0.5
            // (mypaint-tiled-surface.c:813).
            let w = opa_at(rr, 0.5, 0.5);
            let p = sample(px, py);
            sum_r += fix15::to_f32(p[0]) * w;
            sum_g += fix15::to_f32(p[1]) * w;
            sum_b += fix15::to_f32(p[2]) * w;
            sum_a += fix15::to_f32(p[3]) * w;
            sum_w += w;
        }
    }
    if sum_w <= 0.0 {
        return RgbaF32 {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
    }
    // libmypaint's get_color_internal returns STRAIGHT-alpha color, not
    // premultiplied (mypaint-tiled-surface.c:758-765). The accumulator
    // sums premultiplied pixels, then divides RGB by sum_a after the
    // mask-weighted alpha average. hokusai was returning the
    // premultiplied average straight to the caller, so smudge brushes
    // sampling a partially-transparent canvas mixed in artificially
    // dark RGB.
    let alpha = sum_a / sum_w;
    if alpha <= 0.0 {
        return RgbaF32 {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
    }
    RgbaF32 {
        r: (sum_r / sum_a).clamp(0.0, 1.0),
        g: (sum_g / sum_a).clamp(0.0, 1.0),
        b: (sum_b / sum_a).clamp(0.0, 1.0),
        a: alpha.clamp(0.0, 1.0),
    }
}

/// libc `rand()` used by libmypaint's `get_color_pixels_accumulate` for
/// its sparse pixel sampling. The generator and `RAND_MAX` are
/// platform-specific; libmypaint calls the C runtime directly, so parity
/// requires mirroring the same implementation the reference binary links:
///
/// - Windows / MSVC: `holdrand = holdrand * 214013 + 2531011`, returning
///   `(holdrand >> 16) & 0x7fff` with `RAND_MAX == 32767`.
/// - macOS/BSD: Park–Miller 7^5, returning values in `[0, 2^31-1]`.
///
/// libc's state is process-global and seeded with 1 by default; ours is
/// thread-local so brush evaluation stays deterministic and Send. Call
/// [`reset_get_color_rng`] before replaying a stroke that should match an
/// independent libmypaint render.
// Windows 的 C 运行时（msvcrt/ucrt，MinGW 也链接它）用的是 MSVC 的
// LCG；不能按 Rust 的 target_env 判断，否则 MinGW 构建会错误地走
// Park–Miller，既与参考实现不一致，又慢一个数量级。
#[cfg(target_os = "windows")]
const C_RAND_MAX: i32 = 32767;
#[cfg(not(target_os = "windows"))]
const C_RAND_MAX: i32 = 2147483647;

thread_local! {
    static C_RAND_STATE: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

/// Reset the libc-`rand()` mirror to its fresh-process state (seed 1).
/// Call before replaying a stroke that should match an independent
/// libmypaint render.
pub fn reset_get_color_rng() {
    C_RAND_STATE.with(|s| s.set(1));
}

#[cfg(target_os = "windows")]
#[inline]
fn c_rand_step(state: &mut u64) -> i32 {
    // MSVC's `rand()` keeps a 32-bit `holdrand`; the low bits are discarded,
    // only bits 16..30 become the result.
    let next = (*state as u32).wrapping_mul(214013).wrapping_add(2531011);
    *state = next as u64;
    ((next >> 16) & 0x7fff) as i32
}

#[cfg(not(target_os = "windows"))]
#[inline]
fn c_rand_step(state: &mut u64) -> i32 {
    let mut ctx = *state;
    if ctx == 0 {
        ctx = 123459876;
    }
    let hi = (ctx / 127773) as i64;
    let lo = (ctx % 127773) as i64;
    let mut x = 16807 * lo - 2836 * hi;
    if x < 0 {
        x += 0x7fffffff;
    }
    *state = x as u64;
    (x % (C_RAND_MAX as i64 + 1)) as i32
}

/// Sampling RNG 的 `RAND_MAX`（当前平台的 libc `rand()`）。
pub const COLOR_RAND_MAX: i32 = C_RAND_MAX;

/// 当前线程的采样 RNG 状态（供后端并行化 `get_color_pigment` 使用）。
#[inline]
pub fn color_rng_state() -> u64 {
    C_RAND_STATE.with(|s| s.get())
}

/// 写回采样 RNG 状态。
#[inline]
pub fn set_color_rng_state(state: u64) {
    C_RAND_STATE.with(|s| s.set(state));
}

/// 推进一步采样 RNG（与 `get_color_pigment_default` 内部完全一致）。
#[inline]
pub fn color_rand_step(state: &mut u64) -> i32 {
    c_rand_step(state)
}

/// 把采样 RNG 状态向前推进 `steps` 步。
///
/// Windows 的 MSVC LCG 用二进制幂做 O(log steps) 跳进；其他平台退回
/// 逐级推进（并行采样只在这些平台上关闭，见 pigment-surface）。
#[cfg(target_os = "windows")]
pub fn color_rng_advance(mut state: u64, mut steps: u64) -> u64 {
    // 用仿射变换 T(x) = a*x + c 的复合：累积 (A, C) 表示 T^n。
    let (mut acc_a, mut acc_c) = (1u32, 0u32);
    let (mut a_pow, mut c_pow) = (214013u32, 2531011u32);
    let x = state as u32;
    while steps > 0 {
        if steps & 1 == 1 {
            acc_a = acc_a.wrapping_mul(a_pow);
            acc_c = acc_c.wrapping_mul(a_pow).wrapping_add(c_pow);
        }
        c_pow = c_pow.wrapping_mul(a_pow).wrapping_add(c_pow);
        a_pow = a_pow.wrapping_mul(a_pow);
        steps >>= 1;
    }
    state = (acc_a.wrapping_mul(x).wrapping_add(acc_c)) as u64;
    state
}

#[cfg(not(target_os = "windows"))]
pub fn color_rng_advance(mut state: u64, steps: u64) -> u64 {
    for _ in 0..steps {
        c_rand_step(&mut state);
    }
    state
}

#[cfg(test)]
fn c_rand() -> i32 {
    C_RAND_STATE.with(|s| {
        let mut state = s.get();
        let out = c_rand_step(&mut state);
        s.set(state);
        out
    })
}

/// Fill one tile row with the fix15 sampling mask used by `get_color`.
///
/// Geometry is the circular (aspect 1, angle 0) case of `rr_at`, and
/// `opa_at(rr, 0.5, 0.5)` reduces to `1 - rr` for `rr <= 1` (both falloff segments
/// meet at hardness 0.5). Evaluated 8 pixels at a time on AVX2; the arithmetic
/// order matches the scalar path exactly, including the two separate
/// multiplications before the integer truncation.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn mask_row_avx2(
    out: &mut [u32; TILE_SIZE],
    xl: f32,
    yl: f32,
    yp: f32,
    softness: f32,
    x0: i32,
    x1: i32,
    inv_r2: f32,
) {
    use std::arch::x86_64::*;
    let yy = yp + 0.5 - yl;
    let yy2 = yy * yy;
    let soft = _mm256_set1_ps(1.0 - softness);
    let one = _mm256_set1_ps(1.0);
    let scale = _mm256_set1_ps(FIX15_ONE as f32);
    let inv = _mm256_set1_ps(inv_r2);
    let yy2v = _mm256_set1_ps(yy2);
    let xlv = _mm256_set1_ps(xl);
    let lanes = _mm256_setr_ps(0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0);
    let mut xp = x0;
    while xp <= x1 {
        let cnt = (x1 - xp + 1).min(8) as usize;
        if cnt == 8 {
            let base = _mm256_set1_ps(xp as f32);
            let xv = _mm256_add_ps(base, lanes); // xp + lane
            let xx = _mm256_sub_ps(_mm256_add_ps(xv, _mm256_set1_ps(0.5)), xlv);
            let rr = _mm256_mul_ps(_mm256_add_ps(yy2v, _mm256_mul_ps(xx, xx)), inv);
            let opa = _mm256_mul_ps(_mm256_sub_ps(one, rr), soft);
            let maskf = _mm256_mul_ps(opa, scale);
            let valid = _mm256_cmp_ps::<_CMP_LE_OQ>(rr, one);
            let mask = _mm256_and_si256(_mm256_castps_si256(valid), _mm256_cvttps_epi32(maskf));
            let mut tmp = [0u32; 8];
            _mm256_storeu_si256(tmp.as_mut_ptr() as *mut __m256i, mask);
            out[xp as usize..xp as usize + 8].copy_from_slice(&tmp);
            xp += 8;
        } else {
            for cur in xp..=x1 {
                let xx = cur as f32 + 0.5 - xl;
                let rr = (yy2 + xx * xx) * inv_r2;
                let opa = if rr > 1.0 { 0.0 } else { 1.0 - rr };
                let mask = (opa * (1.0 - softness) * FIX15_ONE as f32) as u32;
                out[cur as usize] = mask;
            }
            break;
        }
    }
}

/// Fill `out` with the sampling mask for tile row `yp` and return the
/// horizontal range that can contain non-zero entries.
fn fill_mask_row(
    out: &mut [u32; TILE_SIZE],
    x: f32,
    y: f32,
    radius: f32,
    softness: f32,
    tx: i32,
    ty: i32,
    yp: i32,
) -> Option<(i32, i32)> {
    const HARDNESS: f32 = 0.5;
    let xl = x - (tx * TILE_SIZE as i32) as f32;
    let yl = y - (ty * TILE_SIZE as i32) as f32;
    let r_fringe = radius + 1.0;
    let dy = yp as f32 + 0.5 - yl;
    let rem = r_fringe * r_fringe - dy * dy;
    if rem < 0.0 {
        return None;
    }
    let w = rem.sqrt();
    let x0 = ((xl - w - 1.0).floor() as i32).max(0);
    let x1 = ((xl + w + 1.0).floor() as i32).min(TILE_SIZE as i32 - 1);
    if x0 > x1 {
        return None;
    }
    out.fill(0);
    #[cfg(target_arch = "x86_64")]
    if radius >= 3.0 && crate::spectral::avx2_available() {
        let inv_r2 = 1.0 / (radius * radius);
        unsafe {
            mask_row_avx2(out, xl, yl, yp as f32, softness, x0, x1, inv_r2);
        }
        return Some((x0, x1));
    }
    let inv_r2 = 1.0 / (radius * radius);
    let r_aa_start = if radius < 3.0 {
        let s = if radius > 1.0 { radius - 1.0 } else { 0.0 };
        s * s / 1.0
    } else {
        -1.0
    };
    for xp in x0..=x1 {
        let rr = if r_aa_start >= 0.0 {
            rr_at_aa(
                xp as f32, yp as f32, xl, yl, 1.0, 1.0, 0.0, inv_r2, r_aa_start,
            )
        } else {
            rr_at(xp as f32, yp as f32, xl, yl, 1.0, 1.0, 0.0, inv_r2)
        };
        let opa = opa_at(rr, HARDNESS, softness);
        let mask = (opa * FIX15_ONE as f32) as u32;
        out[xp as usize] = mask;
    }
    Some((x0, x1))
}

/// The per-tile pixel sweep shared by both get_color flavours — mirrors
/// `render_dab_mask`'s geometry exactly: per-tile bbox from
/// `floor(centre ± (radius+1))`, hardness-0.5 falloff, the sub-pixel AA
/// path for radius < 3, and the fix15 mask quantisation (`opa * (1<<15)`
/// truncated; zero entries are skipped without advancing the sample
/// interval counter).
#[inline]
pub fn for_each_mask_pixel<F: FnMut(usize, usize, u32)>(
    x: f32,
    y: f32,
    radius: f32,
    softness: f32,
    tx: i32,
    ty: i32,
    mut f: F,
) {
    let r_fringe = radius + 1.0;
    let yl = y - (ty * TILE_SIZE as i32) as f32;
    let y0 = ((yl - r_fringe).floor() as i32).max(0);
    let y1 = ((yl + r_fringe).floor() as i32).min(TILE_SIZE as i32 - 1);
    let mut masks = [0u32; TILE_SIZE];
    for yp in y0..=y1 {
        let Some((x0, x1)) = fill_mask_row(&mut masks, x, y, radius, softness, tx, ty, yp) else {
            continue;
        };
        for xp in x0..=x1 {
            let mask = masks[xp as usize];
            if mask != 0 {
                f(xp as usize, yp as usize, mask);
            }
        }
    }
}

/// Average color in a circle of `radius` around `(x, y)`, mask-weighted
/// with the same falloff a hardness=0.5 dab produces — bit-exact port of
/// libmypaint's legacy sampler (`get_color_pixels_legacy`), which
/// accumulates per tile in u32 fix15 arithmetic (`opa * channel >> 15`
/// with truncation) before widening to float. Uses
/// [`TiledSurface::tile_lookup`] for read-only sampling.
pub fn get_color_default<S: TiledSurface + ?Sized>(
    surface: &S,
    x: f32,
    y: f32,
    radius: f32,
) -> RgbaF32 {
    const ONE: u32 = FIX15_ONE;
    let radius = if radius < 1.0 { 1.0 } else { radius };
    let r_fringe = radius + 1.0;
    let tx0 = ((x - r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let tx1 = ((x + r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let ty0 = ((y - r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let ty1 = ((y + r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);

    let mut sum_weight = 0.0_f32;
    let mut sum_r = 0.0_f32;
    let mut sum_g = 0.0_f32;
    let mut sum_b = 0.0_f32;
    let mut sum_a = 0.0_f32;

    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let tile_opt = surface.tile_lookup(tx, ty);
            // Per-tile u32 accumulators, like get_color_pixels_legacy.
            let (mut weight, mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for_each_mask_pixel(x, y, radius, 0.5, tx, ty, |lx, ly, mask| {
                weight += mask;
                if let Some(tile) = tile_opt {
                    let p = tile[ly][lx];
                    r += mask * p[0] as u32 / ONE;
                    g += mask * p[1] as u32 / ONE;
                    b += mask * p[2] as u32 / ONE;
                    a += mask * p[3] as u32 / ONE;
                }
            });
            sum_weight += weight as f32;
            sum_r += r as f32;
            sum_g += g as f32;
            sum_b += b as f32;
            sum_a += a as f32;
        }
    }

    if sum_weight <= 0.0 {
        return RgbaF32::TRANSPARENT;
    }
    sum_a /= sum_weight;
    let a_out = sum_a.clamp(0.0, 1.0);
    if sum_a > 0.0 {
        // Legacy sampling un-premultiplies with the weight-normalised
        // alpha (`demul`), clamping against rounding errors.
        RgbaF32 {
            r: (sum_r / sum_weight / sum_a).clamp(0.0, 1.0),
            g: (sum_g / sum_weight / sum_a).clamp(0.0, 1.0),
            b: (sum_b / sum_weight / sum_a).clamp(0.0, 1.0),
            a: a_out,
        }
    } else {
        // All transparent — libmypaint hands back an "ugly" green so bugs
        // show up; the colour must round-trip identically through the
        // smudge bucket cache.
        RgbaF32 {
            r: 0.0,
            g: 1.0,
            b: 0.0,
            a: a_out,
        }
    }
}

/// Port of libmypaint's `Surface2::get_color_pigment`
/// (`get_color_pixels_accumulate` with `paint >= 0`): a mask-weighted
/// running average blending a 10-channel spectral WGM with the
/// alpha-weighted linear average by `paint`. For radii above 2 px the
/// reference only samples every `radius*7`-th masked pixel plus a
/// `1/(7*radius)` random subset (via libc `rand()` — see [`c_rand`]);
/// smaller radii sample every pixel.
///
/// Returns straight-alpha.
pub fn get_color_pigment_default<S: TiledSurface + ?Sized>(
    surface: &S,
    x: f32,
    y: f32,
    radius: f32,
    paint: f32,
) -> RgbaF32 {
    use crate::spectral::{fastpow, rgb_to_spectral, spectral_to_rgb};

    let radius = if radius < 1.0 { 1.0 } else { radius };
    let r_fringe = radius + 1.0;
    let tx0 = ((x - r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let tx1 = ((x + r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let ty0 = ((y - r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);
    let ty1 = ((y + r_fringe).floor() as i32).div_euclid(TILE_SIZE as i32);

    let sample_interval: u32 = if radius <= 2.0 {
        1
    } else {
        (radius * 7.0) as u32
    };
    let random_sample_rate = 1.0_f32 / (7.0 * radius);
    let random_sample_threshold = (random_sample_rate * C_RAND_MAX as f32) as i32;

    let mut sum_weight = 0.0_f32;
    let mut sum_a = 0.0_f32;
    // libmypaint calls `get_color_pixels_accumulate` once per tile with the
    // running `sum_r/g/b`, and that function:
    //   * re-derives its spectral accumulator from those sums,
    //   * runs the per-tile WGM/linear accumulation,
    //   * writes the mixed result back into `sum_r/g/b`.
    // Keep the same tile boundary: carrying the raw spectral accumulator
    // across tiles gives a different (and non-reference) result for sample
    // radii that span more than one 64 px tile.
    let mut sum_r = 0.0_f32;
    let mut sum_g = 0.0_f32;
    let mut sum_b = 0.0_f32;
    // Hoist the libc-rand mirror state out of the per-pixel loop: the mask
    // callback runs once per covered pixel and every call used to touch the
    // thread-local cell. Same sequence, one TLS access per sample.
    let mut rand_state = C_RAND_STATE.with(|s| s.get());

    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let mut avg_spectral = if paint > 0.0 {
                rgb_to_spectral(sum_r, sum_g, sum_b)
            } else {
                [0.0_f32; 10]
            };
            let mut avg_rgb = [sum_r, sum_g, sum_b];
            let tile_opt = surface.tile_lookup(tx, ty);
            // Rolling counter — a fresh one per tile, like the local in
            // get_color_pixels_accumulate. Advances only over non-zero
            // mask entries (zeros are RLE-skipped in the reference).
            let mut interval_counter: u32 = 0;
            for_each_mask_pixel(x, y, radius, 0.5, tx, ty, |lx, ly, mask| {
                if trace_sample() && x == 40.0 && y == 129.0 {
                    let p = surface.tile_lookup(tx, ty).map(|t| t[ly][lx]).unwrap_or([0; 4]);
                    eprintln!(
                        "MASK tx={tx} ty={ty} lx={lx} ly={ly} m={mask} p={:04x},{:04x},{:04x},{:04x}",
                        p[0], p[1], p[2], p[3]
                    );
                }
                let sampled = interval_counter == 0
                    || (sample_interval > 1
                        && c_rand_step(&mut rand_state) < random_sample_threshold);
                if sampled {
                    let p = tile_opt.map(|t| t[ly][lx]).unwrap_or([0, 0, 0, 0]);
                    let pa = p[3] as u32;
                    let a = mask as f32 * pa as f32 / (1u32 << 30) as f32;
                    let alpha_sums = a + sum_a;
                    sum_weight += mask as f32 / FIX15_ONE as f32;
                    let (mut fac_a, mut fac_b) = (1.0_f32, 1.0_f32);
                    if alpha_sums > 0.0 {
                        fac_a = a / alpha_sums;
                        fac_b = 1.0 - fac_a;
                    }
                    if paint > 0.0 && pa > 0 {
                        let spectral = rgb_to_spectral(
                            p[0] as f32 / pa as f32,
                            p[1] as f32 / pa as f32,
                            p[2] as f32 / pa as f32,
                        );
                        for i in 0..10 {
                            avg_spectral[i] =
                                fastpow(spectral[i], fac_a) * fastpow(avg_spectral[i], fac_b);
                        }
                    }
                    if paint < 1.0 && pa > 0 {
                        for i in 0..3 {
                            avg_rgb[i] = p[i] as f32 * fac_a / pa as f32 + avg_rgb[i] * fac_b;
                        }
                    }
                    sum_a += a;
                }
                // Increment-and-wrap instead of `% sample_interval`: the
                // modulus divisor is a runtime value, so `%` compiles to an
                // integer division on every covered pixel (thousands per
                // smudge sample). The counter is always < sample_interval
                // here, so this is exact.
                interval_counter += 1;
                if interval_counter == sample_interval {
                    interval_counter = 0;
                }
            });
            let spec_rgb = spectral_to_rgb(&avg_spectral);
            sum_r = spec_rgb.0 * paint + (1.0 - paint) * avg_rgb[0];
            sum_g = spec_rgb.1 * paint + (1.0 - paint) * avg_rgb[1];
            sum_b = spec_rgb.2 * paint + (1.0 - paint) * avg_rgb[2];
        }
    }

    C_RAND_STATE.with(|s| s.set(rand_state));

    if sum_weight <= 0.0 {
        return RgbaF32::TRANSPARENT;
    }
    sum_a /= sum_weight;

    let a_out = sum_a.clamp(0.0, 1.0);
    if sum_a > 0.0 {
        RgbaF32 {
            r: sum_r.clamp(0.0, 1.0),
            g: sum_g.clamp(0.0, 1.0),
            b: sum_b.clamp(0.0, 1.0),
            a: a_out,
        }
    } else {
        RgbaF32 {
            r: 0.0,
            g: 1.0,
            b: 0.0,
            a: a_out,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AVX2 row masks must be bit-identical to the scalar falloff.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_mask_row_matches_scalar() {
        if !crate::spectral::avx2_available() {
            return;
        }
        for &(xl, yl, radius) in &[
            (10.3f32, 30.7f32, 24.0f32),
            (63.9, -4.25, 58.0),
            (0.5, 0.5, 3.0),
            (-100.2, 70.8, 9.5),
        ] {
            let inv_r2 = 1.0 / (radius * radius);
            let softness = 0.5f32;
            for &yp in &[0.0f32, 7.25, 33.9, 63.0] {
                let x0 = 0i32;
                let x1 = 63i32;
                let mut got = [0u32; TILE_SIZE];
                unsafe {
                    mask_row_avx2(&mut got, xl, yl, yp, softness, x0, x1, inv_r2);
                }
                for xp in x0..=x1 {
                    let yy = yp + 0.5 - yl;
                    let xx = xp as f32 + 0.5 - xl;
                    let rr = (yy * yy + xx * xx) * inv_r2;
                    let want = if rr > 1.0 {
                        0
                    } else {
                        ((1.0 - rr) * (1.0 - softness) * FIX15_ONE as f32) as u32
                    };
                    assert_eq!(
                        got[xp as usize], want,
                        "xl={xl} yl={yl} r={radius} y={yp} x={xp}"
                    );
                }
            }
        }
    }

    #[test]
    fn opa_endpoints() {
        // Center is fully covered; rr=1 is the edge.
        assert!((opa_at(0.0, 0.5, 0.0) - 1.0).abs() < 1e-6);
        assert!(opa_at(1.0, 0.5, 0.0).abs() < 1e-6);
        // At rr=hardness the two segments meet at value `hardness`.
        assert!((opa_at(0.5, 0.5, 0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn lock_alpha_paint_leaves_destination_alpha_untouched() {
        // libmypaint's LockAlpha_Paint never writes rgba[3]
        // (brushmodes.c:481-483); re-premultiplying by the original alpha is
        // what keeps the lock intact. The old code wrote the truncated
        // `opa_a + opa_b * a0` and lost one fix15 unit per dab.
        let dab = Dab {
            x: 10.0,
            y: 10.0,
            radius: 9.0,
            color: RgbaF32::new(1.0, 1.0, 1.0, 1.0),
            opaque: 1.0,
            hardness: 0.9,
            softness: 0.0,
            alpha_eraser: 1.0,
            aspect_ratio: 1.0,
            angle: 0.0,
            lock_alpha: 1.0,
            colorize: 0.0,
            posterize: 0.0,
            posterize_num: 0.0,
            paint: 1.0,
            anti_aliasing: 0.0,
        };
        let passes = DabPasses::new(&dab);
        let a0 = 12_345u16;
        // Premultiplied white at partial alpha: de-premult and spectral mix
        // both stay white, so the premultiplied result must equal a0 exactly.
        let mut pixels = vec![[a0, a0, a0, a0]];
        let entries = [(0usize, FIX15_ONE)];
        blend_lock_alpha_paint(&mut pixels, &entries, &passes, FIX15_ONE / 2);
        assert_eq!(pixels[0][3], a0, "destination alpha must be locked");
        assert_eq!(
            pixels[0][0], a0,
            "white source premultiplied by the original alpha"
        );
    }

    #[test]
    fn hardness_1_is_solid_disk() {
        assert_eq!(opa_at(0.0, 1.0, 0.0), 1.0);
        assert_eq!(opa_at(0.99, 1.0, 0.0), 1.0);
        assert_eq!(opa_at(1.01, 1.0, 0.0), 0.0);
    }

    #[test]
    fn get_color_via_sample_averages_solid_fill() {
        // Sample a 5×5 region painted pure red at full alpha. The
        // mask-weighted average should be ≈ (1, 0, 0, 1).
        let sample = |_px: i32, _py: i32| {
            [
                crate::fix15::FIX15_ONE as u16,
                0,
                0,
                crate::fix15::FIX15_ONE as u16,
            ]
        };
        let c = get_color_via_sample(2.0, 2.0, 1.0, sample);
        assert!((c.r - 1.0).abs() < 1e-3, "red: {}", c.r);
        assert!(c.g.abs() < 1e-3);
        assert!(c.b.abs() < 1e-3);
        assert!((c.a - 1.0).abs() < 1e-3);
    }

    #[test]
    fn rr_increases_with_distance() {
        // Center exactly at pixel center (0.5, 0.5), radius² = 1.
        let near = rr_at(0.0, 0.0, 0.5, 0.5, 1.0, 1.0, 0.0, 1.0);
        let far = rr_at(2.0, 0.0, 0.5, 0.5, 1.0, 1.0, 0.0, 1.0);
        assert!(far > near);
    }

    /// color_a=0 的快捷路径必须与通用 eraser-paint 逐位一致。
    #[test]
    fn eraser_only_paint_fast_path_matches_generic() {
        let dab = Dab {
            x: 10.0,
            y: 10.0,
            radius: 9.0,
            color: RgbaF32 {
                r: 0.1,
                g: 0.5,
                b: 0.9,
                a: 1.0,
            },
            opaque: 0.8,
            hardness: 0.7,
            softness: 0.0,
            alpha_eraser: 0.0,
            aspect_ratio: 2.0,
            angle: 10.0,
            lock_alpha: 0.0,
            colorize: 0.0,
            posterize: 0.0,
            posterize_num: 0.0,
            paint: 1.0,
            anti_aliasing: 0.0,
        };
        let passes = DabPasses::new(&dab);
        let mut pixels = [[0u16; 4]; 256];
        for (i, px) in pixels.iter_mut().enumerate() {
            let a = ((i * 7919) % (FIX15_ONE as usize + 1)) as u16;
            let a = a.min(FIX15_ONE as u16);
            px[3] = a;
            px[0] = (a as u32 * ((i * 13) % 97) as u32 / 97) as u16;
            px[1] = (a as u32 * ((i * 29) % 89) as u32 / 89) as u16;
            px[2] = (a as u32 * ((i * 41) % 83) as u32 / 83) as u16;
        }
        let entries: Vec<(usize, u32)> = (0..256)
            .map(|i| (i, (((i * 349) % 4_000) + 1) as u32))
            .collect();
        let opacity = (0.73 * FIX15_ONE as f32) as u32;
        let mut fast = pixels;
        let mut generic = pixels;
        blend_eraser_only_paint(&mut fast, &entries, &passes, opacity);
        blend_normal_and_eraser_paint(&mut generic, &entries, &passes, 0, opacity);
        assert_eq!(fast, generic);
    }

    /// RNG 跳进必须等于逐步推进。
    #[cfg(target_os = "windows")]
    #[test]
    fn color_rng_advance_matches_stepwise() {
        let mut state = 1u64;
        for steps in [0u64, 1, 2, 3, 7, 31, 255, 4095, 100_000] {
            let mut a = state;
            for _ in 0..steps {
                c_rand_step(&mut a);
            }
            let b = color_rng_advance(state, steps);
            assert_eq!(a, b, "steps={steps}");
            let mut c = state;
            for _ in 0..steps {
                c_rand_step(&mut c);
            }
            state = c;
        }
    }

    #[test]
    fn c_rand_matches_the_platform_crt() {
        reset_get_color_rng();
        #[cfg(target_os = "windows")]
        {
            // Classic MSVC LCG output for seed 1.
            assert_eq!(
                [c_rand(), c_rand(), c_rand(), c_rand()],
                [41, 18467, 6334, 26500]
            );
            assert_eq!(C_RAND_MAX, 32767);
        }
        #[cfg(not(target_os = "windows"))]
        {
            // Park–Miller 7^5 output for seed 1.
            assert_eq!(c_rand(), 16807);
            assert_eq!(C_RAND_MAX, 2147483647);
        }
    }
}
