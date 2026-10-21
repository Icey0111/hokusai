@echo off
setlocal
rem MSVC variant of build_ref.sh. Override any of these if your layout differs:
rem   VCVARS64      path to vcvars64.bat
rem   LIBMYPAINT_DIR  libmypaint checkout (default: ..\..\..\..\_libmypaint_ref)
rem   CREF_OUT_DIR    generated headers + binaries (default: ..\..\..\..\pigment\target\cref)
if "%VCVARS64%"=="" set VCVARS64=C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat
call "%VCVARS64%" || exit /b 1

set HERE=%~dp0
set HERE=%HERE:~0,-1%
set ROOT=%HERE%\..\..\..\..
if "%LIBMYPAINT_DIR%"=="" set LIBMYPAINT_DIR=%ROOT%\_libmypaint_ref
if "%CREF_OUT_DIR%"=="" set CREF_OUT_DIR=%ROOT%\pigment\target\cref
set REF=%LIBMYPAINT_DIR%
set OUT=%CREF_OUT_DIR%

if not exist "%REF%\mypaint-brush.c" (
  echo libmypaint sources not found at "%REF%"
  echo clone https://github.com/mypaint/libmypaint and set LIBMYPAINT_DIR
  exit /b 1
)
if not exist "%OUT%\msvc" mkdir "%OUT%\msvc"

python "%REF%\generate.py" "%OUT%\mypaint-brush-settings-gen.h" "%OUT%\brushsettings-gen.h" || exit /b 1
cd /d "%OUT%\msvc"
cl /nologo /O2 /std:c11 /D_USE_MATH_DEFINES /D_CRT_SECURE_NO_WARNINGS ^
  /I "%HERE%" /I "%REF%" /I "%OUT%" ^
  "%REF%\mypaint.c" "%REF%\mypaint-brush.c" "%REF%\mypaint-brush-settings.c" ^
  "%REF%\mypaint-mapping.c" "%REF%\mypaint-surface.c" "%REF%\mypaint-tiled-surface.c" ^
  "%REF%\brushmodes.c" "%REF%\helpers.c" "%REF%\rng-double.c" "%REF%\tilemap.c" ^
  "%REF%\fifo.c" "%REF%\operationqueue.c" "%REF%\mypaint-symmetry.c" ^
  "%REF%\mypaint-matrix.c" "%REF%\mypaint-rectangle.c" ^
  "%HERE%\ref_surface.c" "%HERE%\ref_runner.c" ^
  /Fe:"%OUT%\ref_runner_msvc.exe"
echo msvc build exit=%ERRORLEVEL%
