#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Makes the archive fixtures in this folder with other tools (Info-ZIP, bsdtar/GNU tar, gzip, Python's zipfile and
tarfile), so the readers are tested against archives they didn't write. Run from anywhere: python3 -I make.py"""

import gzip
import io
import os
import struct
import subprocess
import tarfile
import tempfile
import zipfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
TEXT = ("Files FA6 fixture. " * 200).encode()


def out(name):
    return os.path.join(HERE, name)


def tree(root):
    os.makedirs(os.path.join(root, "Docs/Scans"))
    with open(os.path.join(root, "Docs/Ծրագիր.txt"), "wb") as f:
        f.write(TEXT)
    with open(os.path.join(root, "Docs/Бюджет.csv"), "wb") as f:
        f.write(b"a,b\n1,2\n")
    with open(os.path.join(root, "Docs/Scans/one.bin"), "wb") as f:
        f.write(bytes(range(256)) * 8)
    with open(os.path.join(root, "notes.txt"), "wb") as f:
        f.write(b"hello\n")


def main():
    with tempfile.TemporaryDirectory() as t:
        tree(t)
        files = ["Docs", "notes.txt"]
        # Info-ZIP: deflated, UTF-8 names (flag or 0x7075), and a __MACOSX-style folder
        os.makedirs(os.path.join(t, "__MACOSX"))
        with open(os.path.join(t, "__MACOSX/._notes.txt"), "wb") as f:
            f.write(b"\0" * 32)
        for n in ["infozip.zip", "stored.zip", "stream.zip", "secret.zip"]:
            if os.path.exists(out(n)):
                os.remove(out(n))
        subprocess.run(
            ["zip", "-qrX", out("infozip.zip")] + files + ["__MACOSX"],
            cwd=t,
            check=True,
        )
        subprocess.run(["zip", "-qrX0", out("stored.zip")] + files, cwd=t, check=True)
        # to a pipe: data descriptors after each member
        with open(out("stream.zip"), "wb") as f:
            subprocess.run(["zip", "-qrX", "-"] + files, cwd=t, check=True, stdout=f)
        subprocess.run(
            ["zip", "-qrX", "-P", "secret", out("secret.zip"), "notes.txt"],
            cwd=t,
            check=True,
        )
        # tars: pax and GNU long names (over 100 bytes); no owner names or extended attributes from this machine
        tar = ["tar", "--uid", "0", "--gid", "0", "--uname", "neo", "--gname", "neo", "--no-xattrs", "--no-acls"]
        tar += ["--no-fflags", "--no-mac-metadata"]
        deep = os.path.join(t, "L" + "o" * 60, "n" * 60)
        os.makedirs(deep)
        with open(os.path.join(deep, "g.txt"), "wb") as f:
            f.write(b"long\n")
        both = files + ["L" + "o" * 60]
        subprocess.run(
            tar + ["--format=pax", "-cf", out("pax.tar")] + both, cwd=t, check=True
        )
        subprocess.run(
            tar + ["--format=gnutar", "-cf", out("gnu.tar")] + both, cwd=t, check=True
        )
        subprocess.run(
            tar + ["--format=ustar", "-czf", out("data.tar.gz")] + files,
            cwd=t,
            check=True,
        )

    # a .gz of two members
    with open(out("multi.gz"), "wb") as f:
        f.write(gzip.compress(b"first ", mtime=0))
        f.write(gzip.compress(b"second", mtime=0))

    # CP437: a name byte 0x80 (Ç) without the UTF-8 flag
    b = io.BytesIO()
    with zipfile.ZipFile(b, "w") as z:
        z.writestr("X-cp437.txt", b"cp437")
    out_b = b.getvalue().replace(b"X-cp437", b"\x80-cp437")
    with open(out("cp437.zip"), "wb") as f:
        f.write(out_b)

    # zip-slip names (zipfile and tarfile write them as given)
    b = io.BytesIO()
    with zipfile.ZipFile(b, "w") as z:
        for n in [
            "ok.txt",
            "../evil.txt",
            "a/../../evil2.txt",
            "/abs.txt",
            "C:\\win.txt",
            "a\\..\\..\\evil3.txt",
        ]:
            z.writestr(zipfile.ZipInfo(n), b"x")
    with open(out("slip.zip"), "wb") as f:
        f.write(b.getvalue())
    with tarfile.open(out("slip.tar"), "w", format=tarfile.USTAR_FORMAT) as tf:
        for n in ["ok.txt", "../evil.txt", "/abs.txt"]:
            i = tarfile.TarInfo(n)
            i.size = 1
            tf.addfile(i, io.BytesIO(b"x"))

    # a zip inside a zip (42.zip's shape, one level): never opened
    inner = io.BytesIO()
    with zipfile.ZipFile(inner, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("zeros.bin", b"\0" * (1 << 20))
    with zipfile.ZipFile(out("nested.zip"), "w") as z:
        z.writestr("inner.zip", inner.getvalue())

    # Zip64 end records and 0x0001 extras, by hand (zipfile only writes them past the limits)
    data = TEXT
    comp = zlib.compressobj(6, zlib.DEFLATED, -15)
    packed = comp.compress(data) + comp.flush()
    name = b"big/zip64.txt"
    crc = zlib.crc32(data)
    local = struct.pack(
        "<IHHHHHIIIHH",
        0x04034B50,
        45,
        0,
        8,
        0,
        0x5B73,
        crc,
        0xFFFFFFFF,
        0xFFFFFFFF,
        len(name),
        20,
    )
    local += name + struct.pack("<HHQQ", 1, 16, len(data), len(packed)) + packed
    cen_extra = struct.pack("<HHQQQ", 1, 24, len(data), len(packed), 0)
    cen = (
        struct.pack(
            "<IHHHHHHIIIHHHHHII",
            0x02014B50,
            45,
            45,
            0,
            8,
            0,
            0x5B73,
            crc,
            0xFFFFFFFF,
            0xFFFFFFFF,
            len(name),
            len(cen_extra),
            0,
            0,
            0,
            0,
            0xFFFFFFFF,
        )
        + name
        + cen_extra
    )
    cd_at = len(local)
    e64_at = cd_at + len(cen)
    e64 = struct.pack(
        "<IQHHIIQQQQ", 0x06064B50, 44, 45, 45, 0, 0, 1, 1, len(cen), cd_at
    )
    loc = struct.pack("<IIQI", 0x07064B50, 0, e64_at, 1)
    end = struct.pack(
        "<IHHHHIIH", 0x06054B50, 0, 0, 0xFFFF, 0xFFFF, 0xFFFFFFFF, 0xFFFFFFFF, 0
    )
    with open(out("zip64.zip"), "wb") as f:
        f.write(local + cen + e64 + loc + end)
    print("fixtures written to", HERE)


if __name__ == "__main__":
    main()
