@echo off
setlocal

REM Set RUST_LOG level
set RUST_LOG=info

REM Add DLL directory to PATH
set PATH=%CD%\target\release;%PATH%

echo Starting kernel boot test...
echo.

REM Run the test
target\release\examples\test_kernel_boot.exe 2>&1

echo.
echo Test finished with exit code: %ERRORLEVEL%
