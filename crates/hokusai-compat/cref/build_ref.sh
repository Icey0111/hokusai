#!/usr/bin/env bash
# Build the libmypaint C reference runner without json-c/glib/pkg-config.
# The JSON API is stubbed because ref_runner sets brush values through the
# public C API. Run from Git Bash on Windows.
#
#   LIBMYPAINT_DIR  path to a libmypaint checkout (default: ../_libmypaint_ref
#                   next to this workspace)
#   CREF_OUT_DIR    where to put generated headers and binaries
#                   (default: <workspace-parent>/pigment/target/cref)
#   CC              C compiler command (default: gcc)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../../.." && pwd)"
REF="${LIBMYPAINT_DIR:-$ROOT/_libmypaint_ref}"
OUT="${CREF_OUT_DIR:-$ROOT/pigment/target/cref}"
CC="${CC:-gcc}"

if [ ! -f "$REF/mypaint-brush.c" ]; then
  echo "libmypaint sources not found at: $REF" >&2
  echo "clone https://github.com/mypaint/libmypaint and set LIBMYPAINT_DIR=/path/to/it" >&2
  exit 1
fi
mkdir -p "$OUT"
echo "libmypaint: $REF"
echo "output:     $OUT"

python "$REF/generate.py" \
  "$OUT/mypaint-brush-settings-gen.h" \
  "$OUT/brushsettings-gen.h"

EXE=""
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) EXE=".exe" ;;
esac

CORE=(
  "$REF/mypaint.c"
  "$REF/mypaint-brush.c"
  "$REF/mypaint-brush-settings.c"
  "$REF/mypaint-mapping.c"
  "$REF/mypaint-surface.c"
  "$REF/mypaint-tiled-surface.c"
  "$REF/brushmodes.c"
  "$REF/helpers.c"
  "$REF/rng-double.c"
  "$REF/tilemap.c"
  "$REF/fifo.c"
  "$REF/operationqueue.c"
  "$REF/mypaint-symmetry.c"
  "$REF/mypaint-matrix.c"
  "$REF/mypaint-rectangle.c"
)

FLAGS=(-O2 -std=gnu11 -D_USE_MATH_DEFINES
  -I "$HERE" -I "$REF" -I "$OUT")

"$CC" "${FLAGS[@]}" "${CORE[@]}" \
  "$HERE/ref_surface.c" "$HERE/ref_runner.c" \
  -o "$OUT/ref_runner$EXE" -lm

"$CC" "${FLAGS[@]}" \
  "$REF/mypaint-brush.c" "$REF/mypaint-brush-settings.c" \
  "$REF/mypaint-mapping.c" "$REF/mypaint-surface.c" \
  "$REF/helpers.c" "$REF/rng-double.c" \
  "$HERE/dump_brush.c" \
  -o "$OUT/dump_brush$EXE" -lm

echo "built $OUT/ref_runner$EXE"
echo "build the Rust dump with: cargo build --release -p hokusai-compat --bin dump_fix15"

# Trace variant: same core with CREF_TRACE_COUNT instrumentation in count_dabs_to.
CORE_TRACE=("${CORE[@]/"$REF/mypaint-brush.c"/"$HERE/mypaint-brush-trace.c"}")
CORE_TRACE=("${CORE_TRACE[@]/"$REF/mypaint-surface.c"/"$HERE/mypaint-surface-trace.c"}")
"$CC" "${FLAGS[@]}" "${CORE_TRACE[@]}" \
  "$HERE/ref_surface.c" "$HERE/ref_runner.c" \
  -o "$OUT/ref_runner_trace$EXE" -lm
echo "built $OUT/ref_runner_trace$EXE"
