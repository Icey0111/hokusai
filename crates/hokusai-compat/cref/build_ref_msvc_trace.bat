@echo off
setlocal
rem Trace variant of build_ref_msvc.bat: swaps in the instrumented libmypaint
rem copies (mypaint-brush-trace.c / mypaint-surface-trace.c). Same overrides:
rem   VCVARS64, LIBMYPAINT_DIR, CREF_OUT_DIR
if "%VCVARS64%"=="" set VCVARS64=C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat
call "%VCVARS64%" || exit /b 1

set HERE=%~dp0
set HERE=%HERE:~0,-1%
set ROOT=%HERE%\..\..\..\..
if "%LIBMYPAINT_DIR%"=="" set LIBMYPAINT_DIR=%ROOT%\_libmypaint_ref
if "%CREF_OUT_DIR%"=="" set CREF_OUT_DIR=%ROOT%\pigment\target\cref
set REF=%LIBMYPAINT_DIR%
set OUT=%CREF_OUT_DIR%

if not exist "%REF%\mypaint.c" (
  echo libmypaint sources not found at "%REF%"
  echo clone https://github.com/mypaint/libmypaint and set LIBMYPAINT_DIR
  exit /b 1
)
if not exist "%OUT%\msvc" mkdir "%OUT%\msvc"
if not exist "%OUT%\mypaint-brush-settings-gen.h" python "%REF%\generate.py" "%OUT%\mypaint-brush-settings-gen.h" "%OUT%\brushsettings-gen.h" || exit /b 1
cd /d "%OUT%\msvc"
cl /nologo /O2 /std:c11 /D_USE_MATH_DEFINES /D_CRT_SECURE_NO_WARNINGS ^
  /I "%HERE%" /I "%REF%" /I "%OUT%" ^
  "%REF%\mypaint.c" "%HERE%\mypaint-brush-trace.c" "%REF%\mypaint-brush-settings.c" ^
  "%REF%\mypaint-mapping.c" "%HERE%\mypaint-surface-trace.c" "%REF%\mypaint-tiled-surface.c" ^
  "%REF%\brushmodes.c" "%REF%\helpers.c" "%REF%\rng-double.c" "%REF%\tilemap.c" ^
  "%REF%\fifo.c" "%REF%\operationqueue.c" "%REF%\mypaint-symmetry.c" ^
  "%REF%\mypaint-matrix.c" "%REF%\mypaint-rectangle.c" ^
  "%HERE%\ref_surface.c" "%HERE%\ref_runner.c" ^
  /Fe:"%OUT%\ref_runner_msvc_trace.exe"
echo msvc trace build exit=%ERRORLEVEL%
