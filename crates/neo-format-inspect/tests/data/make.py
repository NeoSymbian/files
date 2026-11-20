#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Makes the inspector fixtures in this folder with other tools, so the readers are tested against files they didn't
write: a DejaVu Sans subset (fonttools' pyftsubset through uvx), certificates (OpenSSL 3.4+ for -not_before), a
synthetic SIS v2 package built field by field as legacy/tools/sis.py's self-test does, and a Java .jad and .jar.
Keys live in a temporary folder and are thrown away. Run from anywhere: python3 -I make.py"""

import os
import shutil
import struct
import subprocess
import tempfile
import zipfile
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
WS = os.path.abspath(os.path.join(HERE, "../../../../.."))  # the NeoSymbian workspace
DEJAVU = os.path.join(WS, "neoui/ui/fonts/DejaVuSans.ttf")


def out(name):
    return os.path.join(HERE, name)


def openssl():
    for o in [
        os.environ.get("OPENSSL"),
        "/opt/homebrew/opt/openssl/bin/openssl",
        "openssl",
    ]:
        if o and shutil.which(o):
            v = subprocess.run([o, "version"], capture_output=True, text=True, check=False).stdout
            if v.startswith("OpenSSL") and not v.startswith(
                (
                    "OpenSSL 1",
                    "OpenSSL 3.0",
                    "OpenSSL 3.1",
                    "OpenSSL 3.2",
                    "OpenSSL 3.3",
                )
            ):
                return o
    raise SystemExit("make.py: needs OpenSSL 3.4+ (set OPENSSL=)")


def font():
    # Latin (Basic + Latin-1 + Extended-A), Cyrillic, Armenian, general punctuation
    subprocess.run(
        ["uvx", "--from", "fonttools", "pyftsubset", DEJAVU,
         "--unicodes=U+0020-007E,U+00A0-017F,U+0400-045F,U+0531-058A,U+2010-2027,U+20AC",
         "--layout-features=", "--no-hinting", "--notdef-outline", "--output-file=" + out("DejaVuSans-subset.ttf")],
        check=True,
    )  # fmt: skip
    shutil.copy(DEJAVU.replace(".ttf", ".ttf.license"), out("DejaVu-LICENSE.txt"))


def certs(t):
    o = openssl()

    def run(*a):
        r = subprocess.run([o, *a], cwd=t, capture_output=True, text=True, check=False)
        if r.returncode:
            raise SystemExit(f"make.py: openssl {a[0]}: {r.stderr}")

    def ext(name, text):
        with open(os.path.join(t, name), "w") as f:
            f.write(text)

    ext(
        "leaf.cnf",
        "basicConstraints=CA:FALSE\nsubjectAltName=DNS:files.notallow.cc,DNS:*.notallow.cc,IP:192.168.3.20\n",
    )
    run("req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", "root.key",
        "-subj", "/C=AM/O=notallow.cc/CN=Home Lab Root CA", "-set_serial", "0x4E45_4F01".replace("_", ""),
        "-not_before", "20260101000000Z", "-not_after", "20351231235959Z", "-config", "/dev/null", "-addext", "basicConstraints=critical,CA:TRUE", "-addext", "keyUsage=critical,keyCertSign,cRLSign", "-sha256", "-out", out("root-ca.pem"))  # fmt: skip
    run(
        "x509", "-in", out("root-ca.pem"), "-outform", "DER", "-out", out("root-ca.der")
    )
    run("req", "-new", "-newkey", "rsa:2048", "-nodes", "-keyout", "leaf.key", "-config", "/dev/null",
        "-subj", "/C=AM/O=notallow.cc/OU=Files/CN=files.notallow.cc", "-out", "leaf.csr")  # fmt: skip
    run("x509", "-req", "-in", "leaf.csr", "-CA", out("root-ca.pem"), "-CAkey", "root.key", "-set_serial", "0x1001",
        "-not_before", "20260301000000Z", "-not_after", "20270301000000Z", "-extfile", "leaf.cnf", "-sha256",
        "-out", out("leaf.pem"))  # fmt: skip
    run("pkey", "-in", "leaf.key", "-pubout", "-out", "leaf.pub")
    # with a key block between the certificates, to skip
    parts = [out("root-ca.pem"), os.path.join(t, "leaf.pub"), out("leaf.pem")]
    with open(out("bundle.pem"), "w") as f:
        f.write("".join(read(n, "r") for n in parts))
    # the signer of the synthetic SIS: Symbian-era dates, long expired
    run("req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes", "-keyout", "sym.key",
        "-subj", "/O=Symbian Software Ltd/CN=Symbian CA I", "-set_serial", "7", "-config", "/dev/null",
        "-not_before", "20060101000000Z", "-not_after", "20120101000000Z", "-outform", "DER", "-out", "sym.der")  # fmt: skip
    with open(out("fingerprints.txt"), "w") as f:
        for name in ["root-ca.pem", "leaf.pem"]:
            fp = subprocess.run([o, "x509", "-noout", "-fingerprint", "-sha256", "-in", out(name)],
                                check=True, capture_output=True, text=True).stdout.strip()  # fmt: skip
            f.write(f"{name} {fp.split('=', 1)[1]}\n")
    return read(os.path.join(t, "sym.der"), "rb")


def read(name, mode):
    with open(name, mode) as f:
        return f.read()


# --- SIS v2 (the field types of the SIS file format; see src/sis.rs) ---
def fld(t, body):
    return struct.pack("<II", t, len(body)) + body + b"\0" * (-len(body) % 4)


def arr(et, items):
    return fld(2, struct.pack("<I", et) + b"".join(struct.pack("<I", len(x)) + x + b"\0" * (-len(x) % 4) for x in items))  # fmt: skip


def s(text):
    return fld(1, text.encode("utf-16-le"))


def sis(signer_der):
    def caps(*bits):
        return fld(41, struct.pack("<I", sum(1 << b for b in bits)))

    hash_ = fld(25, struct.pack("<I", 1) + fld(37, b"\x11" * 20))

    def fd(target, capabilities, index, size):
        # target, MIME type, capabilities, hash, then operation (1 = install), options, length, length, index
        return s(target) + s("") + capabilities + hash_ + struct.pack("<IIQQI", 1, 0, size, size, index)

    # ReadUserData 15, WriteUserData 16, NetworkServices 13, LocalServices 14
    files = [
        fd("!:\\sys\\bin\\xplore.exe", caps(15, 16), 0, 5),
        fd("!:\\sys\\bin\\xpl_net.dll", caps(13, 14), 1, 3),
    ]
    # array elements of SISString: the UTF-16 bytes (the element's length, no type)
    info = (fld(9, struct.pack("<I", 0xA0000BCD)) + s("Lonely Cat Games")
            + arr(1, ["X-plore".encode("utf-16-le")]) + arr(1, ["Lonely Cat Games".encode("utf-16-le")])
            + fld(4, struct.pack("<iii", 1, 58, 0))
            + fld(8, fld(6, struct.pack("<HBB", 2010, 5, 17)) + fld(7, struct.pack("<BBB", 12, 0, 0)))
            + struct.pack("<BB", 0, 0))  # fmt: skip
    dep = fld(9, struct.pack("<I", 0x101F7961)) + arr(
        1, ["Series60v3.0".encode("utf-16-le")]
    )
    prereq = arr(18, [dep]) + arr(18, [])
    block = arr(24, files) + arr(13, []) + arr(26, [])
    signature = fld(38, s("1.2.840.10045.4.3.2")) + fld(37, b"\x22" * 70)
    chain = arr(36, [signature]) + fld(22, fld(37, signer_der))
    ctl = fld(13, fld(14, info) + fld(16, arr(33, [])) + fld(15, arr(11, [struct.pack("<I", 1)])) + fld(17, prereq)
              + fld(19, arr(20, [])) + fld(28, block) + fld(39, chain) + fld(40, struct.pack("<I", 0)))  # fmt: skip
    comp = fld(3, struct.pack("<IQ", 1, len(ctl)) + zlib.compress(ctl))
    payload = [fld(3, struct.pack("<IQ", 0, len(d)) + d) for d in (b"hello", b"net")]
    data = fld(30, arr(31, [arr(32, payload)]))
    contents = (
        fld(34, struct.pack("<H", 0)) + fld(35, struct.pack("<H", 0)) + comp + data
    )
    with open(out("sample.sisx"), "wb") as f:
        f.write(struct.pack("<IIII", 0x10201A7A, 0, 0xA0000BCD, 0) + fld(12, contents))


def java():
    jad = (
        "MIDlet-Name: Pocket Chess\r\n"
        "MIDlet-Vendor: NeoSymbian Tests\r\n"
        "MIDlet-Version: 2.1.0\r\n"
        "MIDlet-Jar-URL: midlet.jar\r\n"
        "MIDlet-Jar-Size: 48213\r\n"
        "MIDlet-1: Pocket Chess, /icon.png, chess.Main\r\n"
        "MicroEdition-Profile: MIDP-2.0\r\n"
        "MicroEdition-Configuration: CLDC-1.1\r\n"
        "MIDlet-Permissions: javax.microedition.io.Connector.http,javax.microedi\r\n"
        " tion.io.Connector.https\r\n"
        "MIDlet-Permissions-Opt: javax.microedition.io.Connector.sms\r\n"
    )
    with open(out("midlet.jad"), "w", newline="") as f:
        f.write(jad)
    manifest = (
        "Manifest-Version: 1.0\n"
        "MIDlet-Name: Pocket Chess\n"
        "MIDlet-Vendor: NeoSymbian Tests\n"
        "MIDlet-Version: 2.1.0\n"
        "MicroEdition-Profile: MIDP-2.1\n"
        "MIDlet-Permissions: javax.microedition.io.Connector.http\n"
    )
    with zipfile.ZipFile(out("midlet.jar"), "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("META-INF/MANIFEST.MF", manifest)
        z.writestr("chess/Main.class", b"\xca\xfe\xba\xbe" + bytes(60))


def main():
    font()
    with tempfile.TemporaryDirectory() as t:
        signer = certs(t)
    sis(signer)
    java()


if __name__ == "__main__":
    main()
