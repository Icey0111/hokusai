# `cref` — differential gate against the real libmypaint C engine

Answers one question with bytes instead of opinions: **does the Rust CPU path
produce exactly the same canvas as libmypaint for the same brush and the same
event stream?**

## How it works

```
            same .myb + same event stream
                  /                    \
   ref_runner (C, libmypaint)      dump_fix15 (Rust, hokusai-core)
   RGBA u16 fix15 buffer           RGBA u16 fix15 buffer
                  \                    /
                    diff_gate.py  -> byte compare
```

- `build_ref.sh` (MinGW/Git Bash) / `build_ref_msvc.bat` / `build_ref_msvc_trace.bat`
  compile libmypaint with minimal `config.h` and `json.h` stubs — **no json-c,
  no GLib, no pkg-config**, which is why this runs on a plain Windows box.
- `ref_runner.c` + `ref_surface.c`: drive the C engine, dump the buffer.
- `../src/bin/dump_fix15.rs`: the same dump from `hokusai-core`
  (`stroke_to_full(..., linear=true)`).
- `gen_inputs.py`: turns a `.myb` into the settings/mapping text that both sides
  consume, and writes the shared event stream.
- `diff_gate.py`: runs both over a whole brush pack and compares
  byte for byte (`--tolerance 0`), optionally emitting a markdown report.
- `mypaint-brush-trace.c` / `mypaint-surface-trace.c`: modified copies of
  libmypaint sources with `CREF_TRACE_*`-gated `fprintf` instrumentation, for
  bisecting a divergence down to the single differing float. Used only for the
  `ref_runner_trace` binary; never linked into the Rust crates. ISC notices
  retained — see [`../../../NOTICE.md`](../../NOTICE.md).

Both sides agree on the call sequence: `new_with_buckets(256)` → `from_defaults`
→ apply brush settings → `begin_atomic` → first event at `dt=10` (warm-up) →
event stream → `end_atomic`. MyPaint's Python wrapper applies
`transform_hsv(hsv, 2.2)` in double before entering the engine; the C reference
side reproduces that in double so the comparison is engine-vs-engine.

## Running it

```sh
# 1. point the scripts at a libmypaint checkout (default: ../_libmypaint_ref)
export LIBMYPAINT_DIR=/path/to/libmypaint CREF_OUT_DIR=/path/to/out
bash  crates/hokusai-compat/cref/build_ref.sh
cargo build --release -p hokusai-compat --bin dump_fix15

# 2. the shared event stream is committed: scripts/brushes196.json
python crates/hokusai-compat/cref/diff_gate.py \
  --brushes /path/to/mypaint-brushes/brushes \
  --script  crates/hokusai-compat/cref/scripts/brushes196.json \
  --work workdir \
  --ref  "$CREF_OUT_DIR/ref_runner.exe" \
  --rust target/release/dump_fix15.exe \
  --tolerance 0 --markdown gate.md
```

## Snapshot (2026-10-21, libmypaint `d5a88f`, `scripts/brushes196.json`)

| Scope | bit-identical | differs | errors |
|---|---|---|---|
| official brush pack | 194 / 196 | 2 | 0 |

The two remaining brushes: `puantilism2.myb` (715 values differ, max 3 LSB) and
`Fan#1.myb` (6 values, max 10 LSB).

Run with both reference binaries on this machine — MinGW `gcc -O2` and MSVC
`cl /O2` — and the outcome is identical: same 194, same two brushes, same
magnitudes. So the numbers are not an artefact of one compiler's float
behaviour.

What the snapshot does **not** cover — read this before quoting 194/196:

- **Events carry tilt, viewzoom, viewrotation and barrel at their defaults.**
  `scripts/brushes196.json` uses 4-field events (`x, y, pressure, dt`), and
  `gen_inputs.py` fills in `xtilt = ytilt = 0`, `viewzoom = 1.0`,
  `viewrotation = 0`, `barrel = 0`, `linear = 1`. Every brush dynamic driven by
  tilt, canvas zoom or canvas rotation is therefore *untested* by this script.
  Adding 10-field event streams that sweep those inputs is the highest-value
  extension.
- **One trajectory, one pressure ramp, 56 events, 512×256.** Dab spacing,
  speed-filter and `partial_dabs` behaviour are exercised only along that path.
- **The `hokusai-compat` fixture PNG goldens are self-generated** (see
  `src/lib.rs`, "Path to true libmypaint parity"), so the ordinary
  `cargo test -p hokusai-compat` run detects regressions, not parity. The C
  gate above is the only parity evidence here.
- The 12 brushes that use `surfacemap_x/y` are included, not skipped: those keys
  are not libmypaint inputs, and both sides ignore unknown inputs, so they
  compare equal rather than being tested.

**These are one script's numbers.** They are a bisectable baseline, not a proof
over the whole event space — the point of the gate is that any change to the
engine either moves these digits or doesn't, reproducibly.
