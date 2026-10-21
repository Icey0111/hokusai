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

# 2. a script is just { width, height, events[] } — see gen_inputs.py
python crates/hokusai-compat/cref/diff_gate.py \
  --brushes /path/to/mypaint-brushes/brushes \
  --script  script.json --work workdir \
  --ref  "$CREF_OUT_DIR/ref_runner.exe" \
  --rust target/release/dump_fix15.exe \
  --tolerance 0 --markdown gate.md
```

## Snapshot (2026-10-21, libmypaint `d5a88fb`, 56 events / 512×256)

| Scope | bit-identical | differs | errors |
|---|---|---|---|
| `hokusai-compat` fixtures | 9 / 9 | 0 | 0 |
| official brush pack | 144 / 196 | 52 | 0 |

Of the 52 diverging brushes: 32 max out at ≤2 LSB (mostly alpha, from
state-filter and float last-bit effects), 20 exceed 2 LSB and cluster in the
smudge / watercolor / oil / acrylic / scatter families, where per-dab state
(`partial_dabs`, speed filters) diverges by 1 ULP and then amplifies. 12 brushes
using `surfacemap_x/y` are skipped and recorded: that key is not a libmypaint
input and the reference asserts on it.

**These are one script's numbers.** They are a bisectable baseline, not a proof
over the whole event space — the point of the gate is that any change to the
engine either moves these digits or doesn't, reproducibly.
