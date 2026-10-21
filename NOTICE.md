# Notice — fork provenance and licensing

Forked from [reearth/hokusai](https://github.com/reearth/hokusai) at upstream
commit `f7e9981` (v0.3.0), maintained as the brush engine of
[mypaint-rs](https://github.com/Icey0111/mypaint-rs), a Rust port of MyPaint.

## License

Unchanged from upstream: **MIT OR Apache-2.0** (`LICENSE-MIT`, `LICENSE-APACHE`).
Copyright (c) Re:Earth and contributors, plus the authors of the reference
material noted below.

Relicensing was *not* needed: the engine is a Rust reimplementation of behaviour
documented by [libmypaint](https://github.com/mypaint/libmypaint), and libmypaint
is **ISC** (its `COPYING` is the ISC permission notice; its README states
"License: ISC"). Pursuing bit-exact parity against an ISC library creates no
copyleft obligation, so keeping the permissive grant preserves the option of
upstreaming these fixes to reearth/hokusai.

## What this fork changes

- Brush-engine parity fixes against libmypaint: 256 smudge buckets + the
  `smudge_bucket` setting, `viewzoom` / `viewrotation` / `barrel_rotation` inputs,
  the linear-sRGB colour chain, `softness`, stroke splitting, tablet `dtime`
  warm-up, RNG persistence across strokes, `DABS_PER_*` evaluation order, signed
  `radius_by_random`, `tilt_declinationx/y` defaults, and the remaining items
  listed in `mypaint-rs/ENGINE_PORT_AUDIT.md`.
- `crates/hokusai-compat/cref/`: a differential gate that renders the same `.myb`
  + event stream through the real libmypaint C engine and through the Rust CPU
  path, then compares RGBA u16 fix15 output byte for byte.

## Reference material inside this repository

- `cref/mypaint-brush-trace.c` and `cref/mypaint-surface-trace.c` are modified
  copies of libmypaint sources (`mypaint-brush.c`, `mypaint-surface.c`) with
  opt-in `fprintf` instrumentation gated on `CREF_TRACE_*` environment variables.
  Original libmypaint copyright and ISC permission notices are retained in their
  headers. They exist only to build the C reference binaries and are never
  compiled into, or linked with, the Rust crates.
- `cref/build_ref.sh` / `cref/build_ref_msvc.bat` otherwise compile a libmypaint
  checkout that **you** point them at; no other upstream C source is stored here.
- `.myb` fixtures under `hokusai/examples/fixtures/` are unmodified copies from
  [mypaint-brushes](https://github.com/mypaint/mypaint-brushes) (CC0 1.0).

## The application layer

`mypaint-rs` is **GPL-3.0-or-later**, and that is where the copyleft obligation
actually sits: it embeds MyPaint's `gui/*.xml` menus and toolbars, which are
GPL-2.0-or-later upstream. Its bundled data keeps its own terms — brush pack
CC0 1.0, icons from MyPaint's `svg/` and `desktop/icons/` public domain.
