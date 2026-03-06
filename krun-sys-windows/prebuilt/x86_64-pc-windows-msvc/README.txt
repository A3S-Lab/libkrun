# Place krun.dll and krun.lib here after building libkrun for Windows.
#
# Build command:
#   cargo build --release -p libkrun --target x86_64-pc-windows-msvc
#
# Then copy:
#   target/x86_64-pc-windows-msvc/release/krun.dll       -> here
#   target/x86_64-pc-windows-msvc/release/krun.dll.lib   -> here as krun.lib
#
# Alternatively, set LIBKRUN_DIR to the release output directory and skip
# copying files into this directory altogether.
