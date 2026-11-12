@echo off
rem Linker shim: Rust armv7-unknown-linux-gnueabi -> zig cc targeting the TV (32-bit ARM, soft-float ABI).
rem glibc 2.17 symbols keep the binary loadable on any webOS 4+ set (the C3 has glibc 2.35).
python -m ziglang cc -target arm-linux-gnueabi.2.17 -mcpu=cortex_a9 %*
