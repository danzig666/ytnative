"""Restore executable bits in an IPK built by ares-package on Windows (NTFS has no +x).

usage: python fix-ipk-modes.py <in.ipk> <out.ipk> <exe-name> [<exe-name> ...]
An IPK is an `ar` archive of debian-binary, control.tar.gz and data.tar.gz; only data.tar.gz is
rewritten, setting mode 0755 on members whose basename is one of the given names.
"""
import gzip
import io
import sys
import tarfile


def read_ar(data: bytes):
    assert data[:8] == b"!<arch>\n", "not an ar archive"
    pos, out = 8, []
    while pos < len(data):
        hdr = data[pos:pos + 60]
        name = hdr[:16].decode().strip().rstrip("/")
        size = int(hdr[48:58].decode().strip())
        out.append((name, data[pos + 60:pos + 60 + size]))
        pos += 60 + size + (size & 1)
    return out


def write_ar(members) -> bytes:
    out = bytearray(b"!<arch>\n")
    for name, body in members:
        out += f"{name:<16}{0:<12}{0:<6}{0:<6}{'100644':<8}{len(body):<10}`\n".encode()
        out += body
        if len(body) & 1:
            out += b"\n"
    return bytes(out)


def main():
    src, dst, *exes = sys.argv[1:]
    members = read_ar(open(src, "rb").read())
    fixed = []
    for name, body in members:
        if name == "data.tar.gz":
            tin = tarfile.open(fileobj=io.BytesIO(body), mode="r:gz")
            buf = io.BytesIO()
            with tarfile.open(fileobj=buf, mode="w:gz", format=tarfile.GNU_FORMAT) as tout:
                for m in tin.getmembers():
                    if m.isfile() and m.name.rsplit("/", 1)[-1] in exes:
                        m.mode = 0o755
                        print(f"  +x {m.name}")
                    tout.addfile(m, tin.extractfile(m) if m.isfile() else None)
            body = buf.getvalue()
        fixed.append((name, body))
    open(dst, "wb").write(write_ar(fixed))
    print(f"wrote {dst}")


if __name__ == "__main__":
    main()
