"""Generate link-only stub shared libraries for the TV target from ci/symbols/<soname>.txt.

Each stub has the real library's DT_SONAME and exports every listed symbol (functions are a bare
`bx lr`, data objects are zero-filled with the real size so copy relocations are sized right). The
app links against these instead of LG's binaries, so:

  * the binary's DT_NEEDED names exactly the sonames below (libGLESv2.so.2, not libmali.so.0: the
    TV's real libGLESv2 pulls in whichever GPU driver that set has), and
  * the build needs no file from a TV -- CI links with nothing but these stubs.

The stubs are never shipped or loaded; at runtime the TV's own libraries are used. Whether every
import really exists on a TV is checked separately against real libraries (`cargo xtask symcheck`,
`cargo xtask fwcompat`).

usage: python tools/gen-stubs.py [--out target/stubs] [--lists ci/symbols] [--force]
"""
import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PLAIN = re.compile(r"^[A-Za-z_.$][A-Za-z0-9_.$]*$")


def sym(name):
    return name if PLAIN.match(name) else '"' + name.replace('"', '\\"') + '"'


def assembly(lines):
    text, data, tls = [], [], []
    for raw in lines:
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        name = sym(parts[0])
        kind = parts[1] if len(parts) > 1 else "func"
        size = int(parts[2]) if len(parts) > 2 else 0
        if kind == "func":
            text.append(f"  .globl {name}\n  .type {name}, %function\n  .p2align 2\n{name}:\n  bx lr\n")
        elif kind == "object":
            data.append(f"  .globl {name}\n  .type {name}, %object\n  .size {name}, {size}\n  .p2align 3\n{name}:\n  .zero {max(size, 1)}\n")
        elif kind == "tls":
            tls.append(f"  .globl {name}\n  .type {name}, %tls_object\n  .size {name}, {size}\n  .p2align 3\n{name}:\n  .zero {max(size, 1)}\n")
        else:
            raise SystemExit(f"gen-stubs: unknown kind {kind!r} in line {raw!r}")
    out = ["  .syntax unified\n  .arm\n  .text\n"] + text
    if data:
        out += ["  .data\n"] + data
    if tls:
        out += ['  .section .tbss,"awT",%nobits\n'] + tls
    return "".join(out), len(text) + len(data) + len(tls)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(REPO / "target" / "stubs"))
    ap.add_argument("--lists", default=str(REPO / "ci" / "symbols"))
    ap.add_argument("--force", action="store_true")
    a = ap.parse_args()
    out, lists = Path(a.out), Path(a.lists)
    out.mkdir(parents=True, exist_ok=True)
    srcs = sorted(lists.glob("*.txt"))
    if not srcs:
        raise SystemExit(f"gen-stubs: no symbol lists in {lists}")
    me = Path(__file__).stat().st_mtime
    for lst in srcs:
        soname = lst.name[: -len(".txt")]
        so = out / soname
        if not a.force and so.exists() and so.stat().st_mtime >= max(lst.stat().st_mtime, me):
            continue
        asm, n = assembly(lst.read_text(encoding="utf-8").splitlines())
        s = out / (soname + ".s")
        s.write_text(asm, encoding="utf-8")
        cmd = [sys.executable, "-m", "ziglang", "cc", "-target", "arm-linux-gnueabi.2.17", "-mcpu=cortex_a9",
               "-shared", "-nostdlib", f"-Wl,-soname,{soname}", "-o", str(so), str(s)]
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode != 0:
            sys.stderr.write(r.stdout + r.stderr)
            raise SystemExit(f"gen-stubs: building {soname} failed")
        os.remove(s)
        print(f"gen-stubs: {soname} ({n} symbols)")


if __name__ == "__main__":
    main()
