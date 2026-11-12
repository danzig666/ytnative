"""zig cc shim for the TV target (32-bit ARM, soft-float ABI, glibc 2.17 symbols).

Used as both the C compiler (cc-rs) and the linker (rustc). Drops the `--target=<rust triple>`
cc-rs adds (zig only understands its own triples) and pins zig's own target.
"""
import subprocess
import sys

args = [a for a in sys.argv[1:] if not a.startswith("--target=")]
cmd = [sys.executable, "-m", "ziglang", "cc", "-target", "arm-linux-gnueabi.2.17", "-mcpu=cortex_a9"] + args
sys.exit(subprocess.call(cmd))
