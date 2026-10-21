//! Color types. Internal pipeline is linear sRGB; HSV is used at brush
//! configuration boundaries (matches libmypaint).

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RgbaF32 {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl RgbaF32 {
    pub const TRANSPARENT: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };

    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hsv {
    pub h: f32, // [0, 1)
    pub s: f32, // [0, 1]
    pub v: f32, // [0, 1]
}

// sRGB transfer fns. libmypaint uses the standard piecewise IEC 61966-2-1.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hsl {
    pub h: f32, // [0, 1)
    pub s: f32, // [0, 1]
    pub l: f32, // [0, 1]
}

// libmypaint uses the GIMP-derived float colour conversions in helpers.c.
// Their rounding points are observable per dab: rgb_to_hsv runs in float
// (with double `+6/+2/+4` and `/6.0`), the hsl helpers and the hsv->rgb
// reconstruction run in double and narrow once per channel.

pub fn rgb_to_hsv(r: f32, g: f32, b: f32) -> Hsv {
    let r = r.clamp(0.0, 1.0);
    let g = g.clamp(0.0, 1.0);
    let b = b.clamp(0.0, 1.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let v = max;
    if delta > 0.0001 {
        let s = delta / max;
        let mut h = if r == max {
            let mut h = (g - b) / delta;
            if h < 0.0 {
                h = (h as f64 + 6.0) as f32;
            }
            h
        } else if g == max {
            (((b - r) / delta) as f64 + 2.0) as f32
        } else {
            (((r - g) / delta) as f64 + 4.0) as f32
        };
        h = (h as f64 / 6.0) as f32;
        Hsv { h, s, v }
    } else {
        Hsv { h: 0.0, s: 0.0, v }
    }
}

pub fn rgb_to_hsl(r: f32, g: f32, b: f32) -> Hsl {
    let r = r.clamp(0.0, 1.0);
    let g = g.clamp(0.0, 1.0);
    let b = b.clamp(0.0, 1.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = ((max as f64 + min as f64) / 2.0) as f32;
    if max == min {
        return Hsl { h: 0.0, s: 0.0, l };
    }
    let delta = max as f64 - min as f64;
    let s = if l <= 0.5 {
        (delta / (max as f64 + min as f64)) as f32
    } else {
        (delta / (2.0 - max as f64 - min as f64)) as f32
    };
    let mut h = if r == max {
        ((g as f64 - b as f64) / delta) as f32
    } else if g == max {
        ((2.0 + (b as f64 - r as f64) / delta)) as f32
    } else {
        ((4.0 + (r as f64 - g as f64) / delta)) as f32
    };
    h = (h as f64 / 6.0) as f32;
    if h < 0.0 {
        h = (h as f64 + 1.0) as f32;
    }
    Hsl { h, s, l }
}

fn hsl_value(n1: f64, n2: f64, mut hue: f64) -> f64 {
    if hue > 6.0 {
        hue -= 6.0;
    } else if hue < 0.0 {
        hue += 6.0;
    }
    if hue < 1.0 {
        n1 + (n2 - n1) * hue
    } else if hue < 3.0 {
        n2
    } else if hue < 4.0 {
        n1 + (n2 - n1) * (4.0 - hue)
    } else {
        n1
    }
}

pub fn hsl_to_rgb(hsl: Hsl) -> RgbaF32 {
    let h = (hsl.h as f64 - (hsl.h as f64).floor()) as f32;
    let s = hsl.s.clamp(0.0, 1.0);
    let l = hsl.l.clamp(0.0, 1.0);
    if s == 0.0 {
        return RgbaF32::new(l, l, l, 1.0);
    }
    let (m1, m2) = if l <= 0.5 {
        let m2 = l as f64 * (1.0 + s as f64);
        (2.0 * l as f64 - m2, m2)
    } else {
        let m2 = l as f64 + s as f64 - l as f64 * s as f64;
        (2.0 * l as f64 - m2, m2)
    };
    let hue6 = h as f64 * 6.0;
    RgbaF32::new(
        hsl_value(m1, m2, hue6 + 2.0) as f32,
        hsl_value(m1, m2, hue6) as f32,
        hsl_value(m1, m2, hue6 - 2.0) as f32,
        1.0,
    )
}

pub fn hsv_to_rgb(hsv: Hsv) -> RgbaF32 {
    let h = (hsv.h as f64 - (hsv.h as f64).floor()) as f32;
    let s = hsv.s.clamp(0.0, 1.0);
    let v = hsv.v.clamp(0.0, 1.0);
    if s == 0.0 {
        return RgbaF32::new(v, v, v, 1.0);
    }
    let mut hue = h as f64;
    if hue == 1.0 {
        hue = 0.0;
    }
    hue *= 6.0;
    let i = hue as i32;
    let f = hue - i as f64;
    let s = s as f64;
    let v = v as f64;
    let w = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i {
        0 => (v, t, w),
        1 => (q, v, w),
        2 => (w, v, t),
        3 => (w, q, v),
        4 => (t, w, v),
        _ => (v, w, q),
    };
    RgbaF32::new(r as f32, g as f32, b as f32, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn hsv_roundtrip() {
        for &(r, g, b) in &[
            (1.0, 0.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0),
            (0.3, 0.6, 0.9),
            (0.5, 0.5, 0.5),
        ] {
            let back = hsv_to_rgb(rgb_to_hsv(r, g, b));
            assert!(
                approx(back.r, r) && approx(back.g, g) && approx(back.b, b),
                "HSV roundtrip lost ({r},{g},{b}) → ({},{},{})",
                back.r,
                back.g,
                back.b
            );
        }
    }

    #[test]
    fn hsl_roundtrip() {
        for &(r, g, b) in &[
            (1.0, 0.0, 0.0),
            (0.0, 1.0, 0.5),
            (0.2, 0.4, 0.7),
            (0.5, 0.5, 0.5),
            (0.0, 0.0, 0.0),
        ] {
            let back = hsl_to_rgb(rgb_to_hsl(r, g, b));
            assert!(
                approx(back.r, r) && approx(back.g, g) && approx(back.b, b),
                "HSL roundtrip lost ({r},{g},{b}) → ({},{},{})",
                back.r,
                back.g,
                back.b
            );
        }
    }
}
