// The SPECTRAL_{R,G,B} tables below are libmypaint-sourced constants written
// at higher precision than f32 can store — the extra digits document the
// upstream value, even though they round identically. Don't lint them.
#![allow(clippy::excessive_precision)]

//! Spectral upsampling / downsampling for libmypaint's pigment-style
//! "paint" blend mode.
//!
//! libmypaint represents pigment mixing in 10 spectral channels (a
//! reduced approximation of the full 36-channel reflectance curves
//! Scharf / Sigg / Hege publish). Mixing two reflectances with a
//! weighted geometric mean (WGM) closely matches how real paint
//! mixes — which RGB linear-light mixing fails to replicate, most
//! visibly for "blue + yellow → green" or "red + green → muddy
//! brown".
//!
//! Tables come straight from `libmypaint/helpers.c` v1.6.1
//! (`T_MATRIX_SMALL`, `spectral_r_small`, etc.). The WGM offset
//! `WGM_EPSILON = 0.001` matches `helpers.h`.
//!
//! Inputs and outputs are straight-alpha linear sRGB in `[0, 1]`.

const WGM_EPSILON: f32 = 0.001;

/// Approximate `log2(x)` matching libmypaint's `fastlog2` (from Paul
/// Mineiro's fastapprox library — vendored into libmypaint via
/// `brushlib/fastapprox/fastlog.h`). Used so hokusai's WGM spectral
/// mixing produces the same numerical output as libmypaint's
/// `fastpow` chain.
#[inline]
pub(crate) fn fastlog2(x: f32) -> f32 {
    let vx = x.to_bits();
    let mx = f32::from_bits((vx & 0x007F_FFFF) | 0x3F00_0000);
    let y = vx as f32 * 1.1920928955078125e-7_f32;
    y - 124.22551499_f32
        - 1.498030302_f32 * mx
        - 1.72587999_f32 / (0.3520887068_f32 + mx)
}

/// Approximate `2^p` matching libmypaint's `fastpow2`.
#[inline]
fn fastpow2(p: f32) -> f32 {
    let offset = if p < 0.0 { 1.0 } else { 0.0 };
    let clipp = if p < -126.0 { -126.0 } else { p };
    let w = clipp as i32;
    let z = clipp - w as f32 + offset;
    let bits = ((1 << 23) as f32
        * (clipp + 121.2740575_f32 + 27.7280233_f32 / (4.84252568_f32 - z)
            - 1.49012907_f32 * z)) as u32;
    f32::from_bits(bits)
}

/// Approximate `x.powf(p)` matching libmypaint's `fastpow` — relative
/// error ≈ 2 % per call, but exactly the same arithmetic libmypaint
/// uses in `mix_colors` and `get_color_pixels_accumulate`, so the
/// spectral path produces bit-identical output to the reference.
#[inline]
pub fn fastpow(x: f32, p: f32) -> f32 {
    fastpow2(p * fastlog2(x))
}

/// `fastpow(x, p)` with `fastlog2(x)` precomputed.
///
/// Used by the per-pixel pigment blend: the dab's source colour is constant,
/// so its 10 logarithms can be hoisted out of the pixel loop. The result is
/// bit-identical to [`fastpow`] because the same `p * log2(x)` product is
/// evaluated with the same inputs.
#[inline]
pub(crate) fn fastpow_from_log(log2_base: f32, p: f32) -> f32 {
    fastpow2(p * log2_base)
}

const SPECTRAL_R: [f32; 10] = [
    0.009281362787953,
    0.009732627042016,
    0.011254252737167,
    0.015105578649573,
    0.024797924177217,
    0.083622585502406,
    0.977865045723212,
    1.000000000000000,
    0.999961046144372,
    0.999999992756822,
];

const SPECTRAL_G: [f32; 10] = [
    0.002854127435775,
    0.003917589679914,
    0.012132151699187,
    0.748259205918013,
    1.000000000000000,
    0.865695937531795,
    0.037477469241101,
    0.022816789725717,
    0.021747419446456,
    0.021384940572308,
];

const SPECTRAL_B: [f32; 10] = [
    0.537052150373386,
    0.546646402401469,
    0.575501819073983,
    0.258778829633924,
    0.041709923751716,
    0.012662638828324,
    0.007485593127390,
    0.006766900622462,
    0.006699764779016,
    0.006676219883241,
];

const T_MATRIX: [[f32; 10]; 3] = [
    [
        0.026595621243689,
        0.049779426257903,
        0.022449850859496,
        -0.218453689278271,
        -0.256894883201278,
        0.445881722194840,
        0.772365886289756,
        0.194498761382537,
        0.014038157587820,
        0.007687264480513,
    ],
    [
        -0.032601672674412,
        -0.061021043498478,
        -0.052490001018404,
        0.206659098273522,
        0.572496335158169,
        0.317837248815438,
        -0.021216624031211,
        -0.019387668756117,
        -0.001521339050858,
        -0.000835181622534,
    ],
    [
        0.339475473216284,
        0.635401374177222,
        0.771520797089589,
        0.113222640692379,
        -0.055251113343776,
        -0.048222578468680,
        -0.012966666339586,
        -0.001523814504223,
        -0.000094718948810,
        -0.000051604594741,
    ],
];

/// Upsample straight-alpha linear sRGB `(r, g, b)` into 10 spectral
/// channels. `WGM_EPSILON` is mixed in to avoid the all-zero / all-one
/// degenerate cases that `pow(x, fac)` can't handle later.
pub fn rgb_to_spectral(r: f32, g: f32, b: f32) -> [f32; 10] {
    let offset = 1.0f32 - WGM_EPSILON;
    // helpers.c uses the double literal `0.001` here, so the addition runs
    // in double and is rounded to float on assignment.
    let r = ((r * offset) as f64 + 0.001) as f32;
    let g = ((g * offset) as f64 + 0.001) as f32;
    let b = ((b * offset) as f64 + 0.001) as f32;
    let mut out = [0.0_f32; 10];
    for i in 0..10 {
        out[i] = SPECTRAL_R[i] * r + SPECTRAL_G[i] * g + SPECTRAL_B[i] * b;
    }
    out
}

/// Downsample 10 spectral channels back to straight-alpha linear sRGB.
/// Negative `T_MATRIX` rows can push the result below `WGM_EPSILON`;
/// the `(tmp - WGM_EPSILON) / offset` step undoes the upsample epsilon
/// and clamps the result into `[0, 1]`.
#[inline]
fn spectral_out_exact(v: f32) -> f32 {
    // C computes `float offset = 1.0 - 0.001` (double arithmetic rounded to
    // float), then `(tmp - 0.001) / offset` promotes both operands to double
    // before the division. Keeping the float-rounded offset is significant:
    // using a double 0.999 shifts the result by one ULP.
    let offset = (1.0f64 - 0.001) as f32 as f64;
    (((v as f64 - 0.001) / offset).clamp(0.0, 1.0)) as f32
}

pub fn spectral_to_rgb(spec: &[f32; 10]) -> (f32, f32, f32) {
    let mut tmp = [0.0_f32; 3];
    for i in 0..10 {
        tmp[0] += T_MATRIX[0][i] * spec[i];
        tmp[1] += T_MATRIX[1][i] * spec[i];
        tmp[2] += T_MATRIX[2][i] * spec[i];
    }
    (
        spectral_out_exact(tmp[0]),
        spectral_out_exact(tmp[1]),
        spectral_out_exact(tmp[2]),
    )
}

/// Port of libmypaint's `mix_colors` (helpers.c) — blends two RGBA
/// straight-alpha colours `a` and `b` (the "current smudge state" and
/// the "sampled / brush" colour) with weight `fac` for `a`. When
/// `paint_mode > 0` the colour channels mix in spectral space via WGM;
/// when `paint_mode < 1` a linear mix is layered in proportionally so a
/// fractional setting fades smoothly between the two.
///
/// Returns `[r, g, b, a]` straight-alpha.
pub fn mix_colors(a: [f32; 4], b: [f32; 4], fac: f32, paint_mode: f32) -> [f32; 4] {
    let opa_a = fac;
    let opa_b = 1.0 - opa_a;
    let out_a = (opa_a * a[3] + opa_b * b[3]).clamp(0.0, 1.0);

    let sfac_a = if a[3] == 0.0 {
        0.0
    } else {
        opa_a * a[3] / (a[3] + b[3] * opa_b)
    };
    let sfac_b = 1.0 - sfac_a;

    let mut result = [0.0_f32; 4];
    if paint_mode > 0.0 {
        let spec_a = rgb_to_spectral(a[0], a[1], a[2]);
        let spec_b = rgb_to_spectral(b[0], b[1], b[2]);
        let mut mix = [0.0_f32; 10];
        for i in 0..10 {
            // helpers.c `mix_colors` deliberately uses the REAL powf
            // ("called infrequently enough that we can afford to not use
            // the faster approximations") — unlike the per-pixel blend
            // loops in brushmodes.c, which do use fastpow. Running fastpow
            // here shifted every smudge-bucket mix by its ~2 % error, and
            // the bucket → dab colour → canvas → resample feedback loop
            // amplified that into a visible colour drift (Round#1).
            mix[i] = spec_a[i].powf(sfac_a) * spec_b[i].powf(sfac_b);
        }
        let (r, g, b_) = spectral_to_rgb(&mix);
        result[0] = r;
        result[1] = g;
        result[2] = b_;
    }
    if paint_mode < 1.0 {
        for i in 0..3 {
            let lin = a[i] * opa_a + b[i] * opa_b;
            result[i] = result[i] * paint_mode + (1.0 - paint_mode) * lin;
        }
    }
    result[3] = out_a;
    result
}

/// Sigmoid-ish factor libmypaint uses to fade between additive and
/// spectral blending at low canvas alpha. `x` is the destination
/// alpha in `[0, 1]`; returns a value in roughly the same range with
/// a smooth transition centred near `x ≈ 0.375`.
pub fn spectral_blend_factor(x: f32) -> f32 {
    const VER_FAC: f32 = 1.65;
    const HOR_FAC: f32 = 8.0;
    const HOR_OFFS: f32 = 3.0;
    let b = x * HOR_FAC - HOR_OFFS;
    (0.5 + b / (1.0 + b.abs() * VER_FAC)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-tripping the primaries should give back something close
    /// to the input. With the WGM offset there's a small contraction.
    #[test]
    fn primaries_round_trip_approximately() {
        for &(r, g, b) in &[(1.0_f32, 0.0, 0.0), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0)] {
            let s = rgb_to_spectral(r, g, b);
            let (r2, g2, b2) = spectral_to_rgb(&s);
            assert!((r2 - r).abs() < 0.05, "r: {r} → {r2}");
            assert!((g2 - g).abs() < 0.05, "g: {g} → {g2}");
            assert!((b2 - b).abs() < 0.05, "b: {b} → {b2}");
        }
    }

    /// The AVX2 batch must reproduce the scalar WGM path bit-for-bit.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_mix_matches_scalar_bitwise() {
        if !avx2_available() {
            return;
        }
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1 << 24) as f32
        };
        for _ in 0..64 {
            let src = [next(), next(), next()];
            let mut log_a = [0.0f32; 10];
            let spec_a = rgb_to_spectral(src[0], src[1], src[2]);
            for i in 0..10 {
                log_a[i] = fastlog2(spec_a[i]);
            }
            let mut fac_a = [0.0f32; 8];
            let mut fac_b = [0.0f32; 8];
            let mut spec_b = [[0.0f32; 10]; 8];
            for p in 0..8 {
                fac_a[p] = next() * 0.5 + 0.3;
                fac_b[p] = 1.0 - fac_a[p];
                let dst = [next(), next(), next()];
                spec_b[p] = rgb_to_spectral(dst[0], dst[1], dst[2]);
            }
            let mut channels = [[0.0f32; 8]; 10];
            for p in 0..8 {
                for i in 0..10 {
                    channels[i][p] = spec_b[p][i];
                }
            }
            let (sr, sg, sb) = unsafe { mix_spectral8(&log_a, &fac_a, &fac_b, &channels) };
            for p in 0..8 {
                let mut mix = [0.0f32; 10];
                for i in 0..10 {
                    mix[i] = fastpow_from_log(log_a[i], fac_a[p]) * fastpow(spec_b[p][i], fac_b[p]);
                }
                let (r, g, b) = spectral_to_rgb(&mix);
                assert_eq!(r.to_bits(), sr[p].to_bits(), "pixel {p} red");
                assert_eq!(g.to_bits(), sg[p].to_bits(), "pixel {p} green");
                assert_eq!(b.to_bits(), sb[p].to_bits(), "pixel {p} blue");
            }
        }
    }

    /// eraser-only AVX2 路径必须与标量 WGM 逐位一致。
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_mix_single_matches_scalar_bitwise() {
        if !avx2_available() {
            return;
        }
        let mut seed = 0x9E37_79B9u32;
        let mut next = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            (seed >> 8) as f32 / (1 << 24) as f32
        };
        for _ in 0..64 {
            let src = [next(), next(), next()];
            let mut log_a = [0.0f32; 10];
            let mut p0 = [0.0f32; 10];
            let spec_a = rgb_to_spectral(src[0], src[1], src[2]);
            for i in 0..10 {
                log_a[i] = fastlog2(spec_a[i]);
                p0[i] = fastpow_from_log(log_a[i], 0.0);
            }
            let mut spec_b = [[0.0f32; 8]; 10];
            let mut dsts = [[0.0f32; 3]; 8];
            for p in 0..8 {
                let dst = [next(), next(), next()];
                dsts[p] = dst;
                let spec = rgb_to_spectral(dst[0], dst[1], dst[2]);
                for i in 0..10 {
                    spec_b[i][p] = spec[i];
                }
            }
            let (sr, sg, sb) = unsafe { mix_spectral8_single(&p0, &spec_b) };
            for p in 0..8 {
                let mut mix = [0.0f32; 10];
                for i in 0..10 {
                    mix[i] = p0[i] * fastpow(spec_b[i][p], 1.0);
                }
                let (r, g, b) = spectral_to_rgb(&mix);
                assert_eq!(r.to_bits(), sr[p].to_bits(), "pixel {p} red");
                assert_eq!(g.to_bits(), sg[p].to_bits(), "pixel {p} green");
                assert_eq!(b.to_bits(), sb[p].to_bits(), "pixel {p} blue");
                let _ = dsts[p];
            }
        }
    }

    /// Blue + yellow ≈ green (a classic pigment-mixing test). With
    /// fac_a = fac_b = 0.5 the WGM blend should look much more green
    /// than the linear-RGB midpoint would.
    #[test]
    fn blue_plus_yellow_is_greenish() {
        let blue = rgb_to_spectral(0.0, 0.0, 1.0);
        let yellow = rgb_to_spectral(1.0, 1.0, 0.0);
        let mut mix = [0.0_f32; 10];
        for i in 0..10 {
            mix[i] = blue[i].powf(0.5) * yellow[i].powf(0.5);
        }
        let (r, g, b) = spectral_to_rgb(&mix);
        // Green channel should dominate, with red and blue both lower.
        assert!(g > r, "green {g} should beat red {r}");
        assert!(g > b, "green {g} should beat blue {b}");
    }
}

// ---------------------------------------------------------------------------
// AVX2 batch path for the per-pixel pigment blend.
//
// The spectral WGM accounts for ~80 % of the rasteriser time on palette-knife
// dabs. Every per-pixel step is a pure lane-local float operation (mul/add/div
// and bit casts), so 8 pixels can be evaluated together without changing any
// rounding: each SIMD instruction applies exactly the scalar operation to its
// lane. The scalar path stays as the reference (and fallback for non-x86_64 /
// pre-AVX2 CPUs); a unit test keeps both bit-identical.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[inline]
pub(crate) fn avx2_available() -> bool {
    std::is_x86_feature_detected!("avx2")
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn fastlog2_8(x: std::arch::x86_64::__m256) -> std::arch::x86_64::__m256 {
    use std::arch::x86_64::*;
    let vx = _mm256_castps_si256(x);
    let mant = _mm256_and_si256(vx, _mm256_set1_epi32(0x007F_FFFF));
    let mx = _mm256_castsi256_ps(_mm256_or_si256(mant, _mm256_set1_epi32(0x3F00_0000)));
    let y = _mm256_mul_ps(_mm256_cvtepi32_ps(vx), _mm256_set1_ps(1.1920928955078125e-7));
    let t = _mm256_sub_ps(
        _mm256_sub_ps(y, _mm256_set1_ps(124.22551499)),
        _mm256_mul_ps(_mm256_set1_ps(1.498030302), mx),
    );
    _mm256_sub_ps(
        t,
        _mm256_div_ps(
            _mm256_set1_ps(1.72587999),
            _mm256_add_ps(_mm256_set1_ps(0.3520887068), mx),
        ),
    )
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn fastpow2_8(p: std::arch::x86_64::__m256) -> std::arch::x86_64::__m256 {
    use std::arch::x86_64::*;
    let zero = _mm256_setzero_ps();
    let offset = _mm256_and_ps(_mm256_cmp_ps::<_CMP_LT_OQ>(p, zero), _mm256_set1_ps(1.0));
    let clipp = _mm256_max_ps(p, _mm256_set1_ps(-126.0));
    let w = _mm256_cvttps_epi32(clipp);
    let z = _mm256_add_ps(_mm256_sub_ps(clipp, _mm256_cvtepi32_ps(w)), offset);
    let denom = _mm256_sub_ps(_mm256_set1_ps(4.84252568), z);
    let inner = _mm256_sub_ps(
        _mm256_add_ps(
            _mm256_add_ps(clipp, _mm256_set1_ps(121.2740575)),
            _mm256_div_ps(_mm256_set1_ps(27.7280233), denom),
        ),
        _mm256_mul_ps(_mm256_set1_ps(1.49012907), z),
    );
    let bitsf = _mm256_max_ps(_mm256_mul_ps(_mm256_set1_ps((1 << 23) as f32), inner), zero);
    _mm256_castsi256_ps(_mm256_cvttps_epi32(bitsf))
}

/// `color_a == 0` 的 eraser-only WGM：源色项恒为 `spec_a^0`（常量）。
///
/// 每通道只剩 `spec_b^1` 一侧；与 [`mix_spectral8`] 使用相同的
/// lane-wise fastlog/fastpow，因此结果与标量
/// `fastpow_from_log(log_a, 0) * fastpow(spec_b, 1)` 逐位一致。
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn mix_spectral8_single(
    spec_a_zero_pow: &[f32; 10],
    spec_b: &[[f32; 8]; 10],
) -> ([f32; 8], [f32; 8], [f32; 8]) {
    use std::arch::x86_64::*;
    let mut tmp0 = _mm256_setzero_ps();
    let mut tmp1 = _mm256_setzero_ps();
    let mut tmp2 = _mm256_setzero_ps();
    for i in 0..10 {
        let lb = fastlog2_8(_mm256_loadu_ps(spec_b[i].as_ptr()));
        let pb = fastpow2_8(lb);
        let mix = _mm256_mul_ps(_mm256_set1_ps(spec_a_zero_pow[i]), pb);
        tmp0 = _mm256_add_ps(tmp0, _mm256_mul_ps(_mm256_set1_ps(T_MATRIX[0][i]), mix));
        tmp1 = _mm256_add_ps(tmp1, _mm256_mul_ps(_mm256_set1_ps(T_MATRIX[1][i]), mix));
        tmp2 = _mm256_add_ps(tmp2, _mm256_mul_ps(_mm256_set1_ps(T_MATRIX[2][i]), mix));
    }
    let mut t0 = [0.0f32; 8];
    let mut t1 = [0.0f32; 8];
    let mut t2 = [0.0f32; 8];
    _mm256_storeu_ps(t0.as_mut_ptr(), tmp0);
    _mm256_storeu_ps(t1.as_mut_ptr(), tmp1);
    _mm256_storeu_ps(t2.as_mut_ptr(), tmp2);
    let mut r = [0.0f32; 8];
    let mut g = [0.0f32; 8];
    let mut b = [0.0f32; 8];
    for lane in 0..8 {
        // The C downsampler promotes to double; do it per lane so the
        // SIMD path stays bit-identical to the scalar reference.
        r[lane] = spectral_out_exact(t0[lane]);
        g[lane] = spectral_out_exact(t1[lane]);
        b[lane] = spectral_out_exact(t2[lane]);
    }
    (r, g, b)
}

/// Evaluate the WGM pigment mix for eight pixels at once.
///
/// * `log_a` — `fastlog2(spec_a)` for the (constant) dab colour.
/// * `fac_a` / `fac_b` — per-pixel alpha weights.
/// * `spec_b[i][px]` — destination spectral reflectance, channel-major.
///
/// Returns straight linear sRGB for the eight lanes.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn mix_spectral8(
    log_a: &[f32; 10],
    fac_a: &[f32; 8],
    fac_b: &[f32; 8],
    spec_b: &[[f32; 8]; 10],
) -> ([f32; 8], [f32; 8], [f32; 8]) {
    use std::arch::x86_64::*;
    let fa = _mm256_loadu_ps(fac_a.as_ptr());
    let fb = _mm256_loadu_ps(fac_b.as_ptr());
    let mut tmp0 = _mm256_setzero_ps();
    let mut tmp1 = _mm256_setzero_ps();
    let mut tmp2 = _mm256_setzero_ps();
    for i in 0..10 {
        let pa = fastpow2_8(_mm256_mul_ps(fa, _mm256_set1_ps(log_a[i])));
        let lb = fastlog2_8(_mm256_loadu_ps(spec_b[i].as_ptr()));
        let pb = fastpow2_8(_mm256_mul_ps(fb, lb));
        let mix = _mm256_mul_ps(pa, pb);
        tmp0 = _mm256_add_ps(tmp0, _mm256_mul_ps(_mm256_set1_ps(T_MATRIX[0][i]), mix));
        tmp1 = _mm256_add_ps(tmp1, _mm256_mul_ps(_mm256_set1_ps(T_MATRIX[1][i]), mix));
        tmp2 = _mm256_add_ps(tmp2, _mm256_mul_ps(_mm256_set1_ps(T_MATRIX[2][i]), mix));
    }
    let mut t0 = [0.0f32; 8];
    let mut t1 = [0.0f32; 8];
    let mut t2 = [0.0f32; 8];
    _mm256_storeu_ps(t0.as_mut_ptr(), tmp0);
    _mm256_storeu_ps(t1.as_mut_ptr(), tmp1);
    _mm256_storeu_ps(t2.as_mut_ptr(), tmp2);
    let mut r = [0.0f32; 8];
    let mut g = [0.0f32; 8];
    let mut b = [0.0f32; 8];
    for lane in 0..8 {
        // The C downsampler promotes to double; do it per lane so the
        // SIMD path stays bit-identical to the scalar reference.
        r[lane] = spectral_out_exact(t0[lane]);
        g[lane] = spectral_out_exact(t1[lane]);
        b[lane] = spectral_out_exact(t2[lane]);
    }
    (r, g, b)
}
