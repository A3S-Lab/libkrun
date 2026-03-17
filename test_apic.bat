@echo off
setlocal

echo ========================================
echo Testing APIC Improvements
echo ========================================
echo.

REM Set environment
set RUST_LOG=info
set PATH=%CD%\target\release;%PATH%

echo Starting kernel boot test...
echo Output will be saved to test_apic.log
echo.
echo Press Ctrl+C after 10 seconds to stop
echo.

REM Run test and capture output
target\release\examples\test_kernel_boot.exe > test_apic.log 2>&1

echo.
echo Test completed or stopped.
echo.
echo ========================================
echo Analyzing output...
echo ========================================
echo.

REM Show first 50 lines
echo First 50 lines of output:
type test_apic.log | more

echo.
echo ========================================
echo Searching for key patterns...
echo ========================================
echo.

REM Search for LAPIC messages
echo LAPIC messages:
findstr /C:"LAPIC" test_apic.log

echo.
echo STUCK messages:
findstr /C:"STUCK" test_apic.log

echo.
echo Exit messages (first 10):
findstr /C:"Exit #" test_apic.log | more

echo.
echo ========================================
echo Analysis complete. Full log: test_apic.log
echo ========================================
