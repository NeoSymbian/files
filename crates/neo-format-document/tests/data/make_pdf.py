# SPDX-License-Identifier: Apache-2.0
# ruff: noqa: UP031, SIM115 -- PDF syntax reads best as %-formats; the files are short-lived one-shot writes
"""The PDF fixtures of FA11, our own content: `python3 -I make_pdf.py` (stdlib only; on macOS `sips` makes the JPEG
and `cupsfilter` the Quartz-written copy, both kept in git so other machines don't need them).

- boarding-pass.pdf: the board's Ararat Air pass (files2-view-pdf), 2 pages: NeoDoc (DejaVu Sans) subset as Type0
  Identity-H fonts (Latin and an Armenian line), the board's QR as module rects (the matrix in boarding-pass.qr),
  a dashed rule, the airplane as a path, a blue header; page 2 a JPEG (DCTDecode) and a half-transparent box.
- book-100.pdf: 100 pages of Helvetica (not embedded: the standard-14 fallback), an outline of 10 chapters (one with
  two sections, one title in UTF-16), every object in an object stream, a cross-reference stream.
- locked.pdf: RC4 128-bit (R3) with the user password 1234.
- deep.pdf: forms nested 200 deep; loop.pdf: a page tree that loops and a huge MediaBox; broken-xref.pdf: every
  cross-reference offset wrong.
- quartz.pdf: cupsfilter's PDF of a text with Armenian and Cyrillic (macOS Quartz, subset fonts).
"""

import hashlib
import os
import struct
import subprocess
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
FONTS = os.path.join(HERE, "..", "..", "..", "files-ui", "ui", "fonts")


# ---------------------------------------------------------------- TrueType: cmap, advances, a glyph subset


class TTF:
    def __init__(self, path):
        self.b = open(path, "rb").read()
        n = struct.unpack(">H", self.b[4:6])[0]
        self.tables = {}
        for i in range(n):
            tag, _, off, ln = struct.unpack(">4sIII", self.b[12 + 16 * i : 28 + 16 * i])
            self.tables[tag.decode()] = (off, ln)
        head = self.t("head")
        self.upem = struct.unpack(">H", head[18:20])[0]
        self.long_loca = struct.unpack(">h", head[50:52])[0] == 1
        self.hmetrics = struct.unpack(">H", self.t("hhea")[34:36])[0]
        self.glyphs = struct.unpack(">H", self.t("maxp")[4:6])[0]
        self.map = self._cmap()

    def t(self, tag):
        off, ln = self.tables[tag]
        return self.b[off : off + ln]

    def _cmap(self):
        c = self.t("cmap")
        n = struct.unpack(">H", c[2:4])[0]
        subs = {}
        for i in range(n):
            pid, eid, off = struct.unpack(">HHI", c[4 + 8 * i : 12 + 8 * i])
            subs[(pid, eid)] = off
        off = subs.get((3, 10)) or subs.get((3, 1))
        assert off is not None, "no Unicode cmap"
        fmt = struct.unpack(">H", c[off : off + 2])[0]
        m = {}
        if fmt == 12:
            groups = struct.unpack(">I", c[off + 12 : off + 16])[0]
            for g in range(groups):
                s, e, gid = struct.unpack(
                    ">III", c[off + 16 + 12 * g : off + 28 + 12 * g]
                )
                for ch in range(s, e + 1):
                    m[ch] = gid + ch - s
        else:
            segx2 = struct.unpack(">H", c[off + 6 : off + 8])[0]
            ends = off + 14
            starts = ends + segx2 + 2
            deltas = starts + segx2
            ranges = deltas + segx2
            for s in range(segx2 // 2):
                end, start = (
                    struct.unpack(">H", c[ends + 2 * s : ends + 2 * s + 2])[0],
                    struct.unpack(">H", c[starts + 2 * s : starts + 2 * s + 2])[0],
                )
                delta = struct.unpack(">h", c[deltas + 2 * s : deltas + 2 * s + 2])[0]
                ro = struct.unpack(">H", c[ranges + 2 * s : ranges + 2 * s + 2])[0]
                for ch in range(start, end + 1):
                    if ch == 0xFFFF:
                        continue
                    if ro == 0:
                        g = (ch + delta) & 0xFFFF
                    else:
                        p = ranges + 2 * s + ro + 2 * (ch - start)
                        g = struct.unpack(">H", c[p : p + 2])[0]
                        g = (g + delta) & 0xFFFF if g else 0
                    if g:
                        m[ch] = g
        return m

    def gid(self, ch):
        return self.map.get(ord(ch), 0)

    def advance(self, gid):
        h = self.t("hmtx")
        i = min(gid, self.hmetrics - 1)
        return struct.unpack(">H", h[4 * i : 4 * i + 2])[0]

    def width(self, text, size):
        return sum(self.advance(self.gid(c)) for c in text) * size / self.upem

    def _loca(self):
        l = self.t("loca")
        if self.long_loca:
            return [
                struct.unpack(">I", l[4 * i : 4 * i + 4])[0]
                for i in range(self.glyphs + 1)
            ]
        return [
            2 * struct.unpack(">H", l[2 * i : 2 * i + 2])[0]
            for i in range(self.glyphs + 1)
        ]

    def subset(self, gids):
        """The font with only `gids` (and their components) keeping outlines; glyph ids unchanged."""
        loca = self._loca()
        glyf = self.t("glyf")
        keep = set(gids) | {0}
        todo = list(keep)
        while todo:
            g = todo.pop()
            data = glyf[loca[g] : loca[g + 1]]
            if len(data) >= 10 and struct.unpack(">h", data[:2])[0] < 0:
                p = 10
                while True:
                    flags, comp = struct.unpack(">HH", data[p : p + 4])
                    if comp not in keep:
                        keep.add(comp)
                        todo.append(comp)
                    p += 4 + (4 if flags & 1 else 2)
                    p += (
                        8
                        if flags & 0x80
                        else 4
                        if flags & 0x40
                        else 2
                        if flags & 0x8
                        else 0
                    )
                    if not flags & 0x20:
                        break
        new_glyf = b""
        new_loca = []
        for g in range(self.glyphs):
            new_loca.append(len(new_glyf))
            if g in keep:
                d = glyf[loca[g] : loca[g + 1]]
                new_glyf += d + b"\0" * (-len(d) % 4)
        new_loca.append(len(new_glyf))
        head = bytearray(self.t("head"))
        head[8:12] = b"\0\0\0\0"  # checkSumAdjustment
        head[50:52] = struct.pack(">h", 1)
        tables = {
            "head": bytes(head),
            "hhea": self.t("hhea"),
            "hmtx": self.t("hmtx"),
            "maxp": self.t("maxp"),
            "loca": b"".join(struct.pack(">I", o) for o in new_loca),
            "glyf": new_glyf,
        }
        return sfnt(tables)


def sfnt(tables):
    tags = sorted(tables)
    n = len(tags)
    es = max(i for i in range(8) if 2**i <= n)
    out = struct.pack(">IHHHH", 0x00010000, n, 16 * 2**es, es, 16 * n - 16 * 2**es)
    off = 12 + 16 * n
    body = b""
    for tag in tags:
        d = tables[tag]
        csum = (
            sum(struct.unpack(">%dI" % ((len(d) + 3) // 4), d + b"\0" * (-len(d) % 4)))
            & 0xFFFFFFFF
        )
        out += struct.pack(">4sIII", tag.encode(), csum, off + len(body), len(d))
        body += d + b"\0" * (-len(d) % 4)
    return out + body


# ---------------------------------------------------------------- a PDF writer


PAD = bytes.fromhex("28BF4E5E4E758A4164004E56FFFA01082E2E00B6D0683E802F0CA9FE6453697A")


def rc4(key, data):
    s = list(range(256))
    j = 0
    for i in range(256):
        j = (j + s[i] + key[i % len(key)]) & 255
        s[i], s[j] = s[j], s[i]
    i = j = 0
    out = bytearray()
    for b in data:
        i = (i + 1) & 255
        j = (j + s[i]) & 255
        s[i], s[j] = s[j], s[i]
        out.append(b ^ s[(s[i] + s[j]) & 255])
    return bytes(out)


class Pdf:
    def __init__(self):
        self.objs = []

    def reserve(self):
        self.objs.append(None)
        return len(self.objs)

    def set(self, n, body, stream=None):
        self.objs[n - 1] = (
            body if isinstance(body, bytes) else body.encode("latin-1"),
            stream,
        )

    def add(self, body, stream=None):
        n = self.reserve()
        self.set(n, body, stream)
        return n

    def stream(self, d, data, flate=True):
        if flate:
            data = zlib.compress(data, 9)
            d = d + " /Filter /FlateDecode"
        return self.add("<< %s /Length %d >>" % (d, len(data)), data)

    def save(self, path, root, info=None, objstm=False, password=None):
        trailer = "/Root %d 0 R" % root + (" /Info %d 0 R" % info if info else "")
        fid = hashlib.md5(path.encode()).digest()
        key = None
        enc = 0
        if password is not None:
            o = rc4_owner(b"owner", password)
            p = -4
            key = rc4_key(password, o, p, fid)
            u = rc4_user(key, fid)
            enc = self.add(
                "<< /Filter /Standard /V 2 /R 3 /Length 128 /P %d /O <%s> /U <%s> >>"
                % (p, o.hex(), u.hex())
            )
            trailer += " /Encrypt %d 0 R" % enc
        trailer += " /ID [<%s> <%s>]" % (fid.hex(), fid.hex())
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offsets = {}
        if not objstm:
            for i, (body, stream) in enumerate(self.objs, 1):
                if (
                    key
                    and stream is not None
                    and b"/Type /XRef" not in body
                    and i != enc
                ):
                    k = hashlib.md5(key + struct.pack("<I", i)[:3] + b"\0\0").digest()[
                        :16
                    ]
                    stream = rc4(k, stream)
                offsets[i] = len(out)
                out += b"%d 0 obj\n" % i + body
                if stream is not None:
                    out += b"\nstream\n" + stream + b"\nendstream"
                out += b"\nendobj\n"
            xref = len(out)
            out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(self.objs) + 1)
            for i in range(1, len(self.objs) + 1):
                out += b"%010d 00000 n \n" % offsets[i]
            out += (
                "trailer\n<< /Size %d %s >>\nstartxref\n%d\n%%%%EOF\n"
                % (len(self.objs) + 1, trailer, xref)
            ).encode()
        else:
            # Every object without a stream into one object stream, then a cross-reference stream.
            plain = [i for i, (_, s) in enumerate(self.objs, 1) if s is None]
            heads, bodies = [], b""
            for i in plain:
                heads.append(b"%d %d" % (i, len(bodies)))
                bodies += self.objs[i - 1][0] + b"\n"
            head = b" ".join(heads) + b"\n"
            stm = zlib.compress(head + bodies, 9)
            stm_n = len(self.objs) + 1
            self.objs.append(
                (
                    b"<< /Type /ObjStm /N %d /First %d /Filter /FlateDecode /Length %d >>"
                    % (len(plain), len(head), len(stm)),
                    stm,
                )
            )
            for i, (body, stream) in enumerate(self.objs, 1):
                if stream is None:
                    continue
                offsets[i] = len(out)
                out += (
                    b"%d 0 obj\n" % i
                    + body
                    + b"\nstream\n"
                    + stream
                    + b"\nendstream\nendobj\n"
                )
            xref_n = len(self.objs) + 1
            offsets[xref_n] = len(out)
            rows = b"\x00" + struct.pack(">IH", 0, 65535)
            for i in range(1, xref_n + 1):
                if i in offsets:
                    rows += b"\x01" + struct.pack(">IH", offsets[i], 0)
                else:
                    rows += b"\x02" + struct.pack(">IH", stm_n, plain.index(i))
            data = zlib.compress(rows, 9)
            out += (
                b"%d 0 obj\n<< /Type /XRef /Size %d /W [1 4 2] %s /Filter /FlateDecode /Length %d >>\nstream\n"
                % (
                    xref_n,
                    xref_n + 1,
                    trailer.encode(),
                    len(data),
                )
            )
            out += data + b"\nendstream\nendobj\n"
            out += b"startxref\n%d\n%%%%EOF\n" % offsets[xref_n]
        open(os.path.join(HERE, path), "wb").write(out)
        return bytes(out)


def pad(pw):
    return (pw.encode("latin-1") + PAD)[:32]


def rc4_owner(owner, user):
    h = hashlib.md5((owner + PAD)[:32]).digest()
    for _ in range(50):
        h = hashlib.md5(h).digest()
    o = pad(user)
    for i in range(20):
        o = rc4(bytes(b ^ i for b in h[:16]), o)
    return o


def rc4_key(user, o, p, fid):
    h = hashlib.md5(pad(user) + o + struct.pack("<i", p) + fid).digest()
    for _ in range(50):
        h = hashlib.md5(h[:16]).digest()
    return h[:16]


def rc4_user(key, fid):
    u = hashlib.md5(PAD + fid).digest()
    for i in range(20):
        u = rc4(bytes(b ^ i for b in key), u)
    return u + b"\0" * 16


def pdf_text(s):
    """A PDF text string: PDFDocEncoding for ASCII, otherwise UTF-16BE with its mark."""
    if all(ord(c) < 128 for c in s):
        return (
            "(" + s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)") + ")"
        )
    return "<feff" + s.encode("utf-16-be").hex() + ">"


# ---------------------------------------------------------------- boarding-pass.pdf

QR = [
    "1111111011111011001111111",
    "1000001011101101101000001",
    "1011101000010001001011101",
    "1011101001110110001011101",
    "1011101010000001001011101",
    "1000001011001111101000001",
    "1111111000001110001111111",
    "0000000000100011100000000",
    "1000011010011001110010001",
    "0110000100111101110110010",
    "0001111011001000101100000",
    "1000010100001000111111000",
    "1001001010001101110101110",
    "0100011101110011000110110",
    "0010101100110111111101011",
    "1110011010100100010001111",
    "1011100011111110001011111",
    "0000000011100000011010010",
    "1111111000000011111000000",
    "1000001011101000110000111",
    "1011101001011101001100111",
    "1011101000010011011111010",
    "1011101000111110101101100",
    "1000001010011011110010101",
    "1111111011111111000110111",
]

W, H = 420, 595  # A5 in points
BLUE = (0x1F / 255, 0x4E / 255, 0x8C / 255)
INK = (0x16 / 255, 0x18 / 255, 0x1C / 255)
GREY = (0x5F / 255, 0x66 / 255, 0x70 / 255)
# The QR: 25 modules of 3.84 pt, its top edge at y = 282 (y up), modules from the top row down.
QR_X, QR_TOP, QR_M = 20.0, 282.0, 3.84
AIRPLANE = [  # the board's g-airplane (24-unit box), y down
    ("M", 12, 3.2),
    ("C", 12.9, 3.2, 13.5, 4.1, 13.5, 5.2),
    ("L", 13.5, 9.8),
    ("L", 20, 13.6),
    ("L", 20, 15.4),
    ("L", 13.5, 13.4),
    ("L", 13.5, 17.6),
    ("L", 15.5, 19.1),
    ("L", 15.5, 20.6),
    ("L", 12, 19.6),
    ("L", 8.5, 20.6),
    ("L", 8.5, 19.1),
    ("L", 10.5, 17.6),
    ("L", 10.5, 13.4),
    ("L", 4, 15.4),
    ("L", 4, 13.6),
    ("L", 10.5, 9.8),
    ("L", 10.5, 5.2),
    ("C", 10.5, 4.1, 11.1, 3.2, 12, 3.2),
]


def airplane(x, y, size, rotate=False):
    """The airplane filled at (x, y) = its box's bottom left, `size` points, nose up (or right)."""
    k = size / 24
    out = []
    for op, *a in AIRPLANE:
        pts = []
        for i in range(0, len(a), 2):
            px, py = a[i], 24 - a[i + 1]
            if rotate:
                px, py = py, 24 - px
            pts.append("%.2f %.2f" % (x + px * k, y + py * k))
        out.append(" ".join(pts) + {"M": " m", "L": " l", "C": " c"}[op])
    return "\n".join(out) + "\nh f\n"


def boarding_pass():
    reg = TTF(os.path.join(FONTS, "NeoDoc-Regular.ttf"))
    bold = TTF(os.path.join(FONTS, "NeoDoc-Bold.ttf"))
    used = {"R": set(), "B": set()}
    page1, page2 = [], []

    def text(out, face, size, x, y, s, rgb=INK, right=False):
        f = bold if face == "B" else reg
        if right:
            x -= f.width(s, size)
        gids = [f.gid(c) for c in s]
        used[face] |= set(gids)
        out.append(
            "BT /%s %.2f Tf %.3f %.3f %.3f rg %.2f %.2f Td <%s> Tj ET"
            % (face, size, *rgb, x, y, "".join("%04x" % g for g in gids))
        )

    o = page1
    o.append("%.3f %.3f %.3f rg 0 %d %d 64 re f" % (*BLUE, H - 64, W))
    o.append("1 1 1 rg " + airplane(18, H - 44, 23))
    text(o, "B", 18, 46, H - 40, "ARARAT AIR", (1, 1, 1))
    text(o, "B", 12.8, W - 20, H - 38, "BOARDING PASS", (1, 1, 1), right=True)
    text(o, "B", 11, 20, H - 90, "PASSENGER", GREY)
    text(o, "B", 16.6, 20, H - 110, "PETROSYAN / ANI MS")
    text(o, "B", 41, 20, H - 160, "EVN")
    text(o, "R", 13.4, 20, H - 178, "Yerevan Zvartnots", GREY)
    text(o, "B", 41, W - 20, H - 160, "VIE", right=True)
    text(o, "R", 13.4, W - 20, H - 178, "Vienna Schwechat", GREY, right=True)
    o.append(
        "0.722 0.745 0.78 RG 1.9 w [5 3] 0 d 112 %d m 308 %d l S [] 0 d"
        % (H - 147, H - 147)
    )
    o.append("1 1 1 rg 196 %d 28 28 re f" % (H - 161))
    o.append("%.3f %.3f %.3f rg " % BLUE + airplane(198, H - 159, 24, rotate=True))
    for col, (lab, val) in enumerate(
        [("FLIGHT", "RA 214"), ("DATE", "18 NOV 2026"), ("DEPARTS", "06:20")]
    ):
        text(o, "B", 11, 20 + col * 132, H - 210, lab, GREY)
        text(o, "B", 16.6, 20 + col * 132, H - 228, val)
    for col, (lab, val) in enumerate(
        [("BOARDING", "05:40"), ("GATE", "3"), ("SEAT", "14A")]
    ):
        text(o, "B", 11, 20 + col * 132, H - 252, lab, GREY)
        text(o, "B", 21.8, 20 + col * 132, H - 274, val)
    o.append(
        "0.788 0.808 0.839 RG 1.9 w [5 3] 0 d 0 %d m %d %d l S [] 0 d"
        % (H - 248 - 50, W, H - 248 - 50)
    )
    o.append("0.067 0.067 0.067 rg")
    for r, row in enumerate(QR):
        for c, bit in enumerate(row):
            if bit == "1":
                o.append(
                    "%.2f %.2f %.2f %.2f re"
                    % (QR_X + c * QR_M, QR_TOP - (r + 1) * QR_M, QR_M, QR_M)
                )
    o.append("f")
    for i, (lab, val) in enumerate(
        [("GROUP", "B"), ("BOOKING", "K7Q2MX"), ("SEQ", "087")]
    ):
        text(o, "B", 11, 140, QR_TOP - 12 - i * 34, lab, GREY)
        text(o, "B", 16.6, 140, QR_TOP - 30 - i * 34, val)
    text(
        o, "R", 12, 20, 160, "Gate closes 20 minutes before departure. Have your", GREY
    )
    text(o, "R", 12, 20, 144, "passport and this pass ready.", GREY)
    text(o, "B", 16, 20, 104, "Բարի թռիչք։ Хорошего полёта.")

    o = page2
    text(o, "B", 20, 20, H - 50, "Conditions of carriage")
    lines = [
        "Check-in closes 60 minutes before departure. Bags over",
        "23 kg are charged at the airport. Liquids in hand luggage",
        "must be in containers of 100 ml or less.",
        "Մուտքը փակվում է մեկնումից 20 րոպե առաջ։",
    ]
    for i, s in enumerate(lines):
        text(o, "R", 12.5, 20, H - 90 - i * 20, s)
    o.append("q 380 0 0 190 20 200 cm /Im1 Do Q")
    o.append("/Half gs 0.12 0.31 0.55 rg 60 240 300 60 re f")
    text(o, "B", 16, 80, 262, "Mount Ararat at dawn", (1, 1, 1))

    p = Pdf()
    fonts = {}
    for face, f, name in (("R", reg, "NeoDoc-Regular"), ("B", bold, "NeoDoc-Bold")):
        sub = f.subset(used[face])
        file = p.stream("/Length1 %d" % len(sub), sub)
        k = 1000 / f.upem
        desc = p.add(
            "<< /Type /FontDescriptor /FontName /AAAAAA+%s /Flags 32 /FontBBox [-1021 -463 1793 1232] /ItalicAngle 0 "
            "/Ascent 928 /Descent -236 /CapHeight 729 /StemV 80 /FontFile2 %d 0 R >>"
            % (name, file)
        )
        widths = " ".join(
            "%d [%d]" % (g, round(f.advance(g) * k)) for g in sorted(used[face])
        )
        cid = p.add(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /AAAAAA+%s /CIDSystemInfo << /Registry (Adobe) "
            "/Ordering (Identity) /Supplement 0 >> /FontDescriptor %d 0 R /W [%s] /DW 600 /CIDToGIDMap /Identity >>"
            % (name, desc, widths)
        )
        fonts[face] = p.add(
            "<< /Type /Font /Subtype /Type0 /BaseFont /AAAAAA+%s /Encoding /Identity-H /DescendantFonts [%d 0 R] >>"
            % (name, cid)
        )
    jpeg = make_jpeg()
    im = p.add(
        "<< /Type /XObject /Subtype /Image /Width 190 /Height 95 /ColorSpace /DeviceRGB /BitsPerComponent 8 "
        "/Filter /DCTDecode /Length %d >>" % len(jpeg),
        jpeg,
    )
    half = p.add("<< /Type /ExtGState /ca 0.5 >>")
    res = p.add(
        "<< /Font << /R %d 0 R /B %d 0 R >> /XObject << /Im1 %d 0 R >> /ExtGState << /Half %d 0 R >> >>"
        % (fonts["R"], fonts["B"], im, half)
    )
    pages = p.reserve()
    kids = []
    for content in (page1, page2):
        c = p.stream("", "\n".join(content).encode())
        kids.append(
            p.add(
                "<< /Type /Page /Parent %d 0 R /MediaBox [0 0 %d %d] /Resources %d 0 R /Contents %d 0 R >>"
                % (pages, W, H, res, c)
            )
        )
    p.set(
        pages,
        "<< /Type /Pages /Kids [%s] /Count 2 >>" % " ".join("%d 0 R" % k for k in kids),
    )
    root = p.add("<< /Type /Catalog /Pages %d 0 R >>" % pages)
    info = p.add(
        "<< /Title %s /Producer (NeoSymbian make_pdf.py) >>"
        % pdf_text("Boarding pass RA 214")
    )
    p.save("boarding-pass.pdf", root, info)
    open(os.path.join(HERE, "boarding-pass.qr"), "w").write(
        "# the QR of boarding-pass.pdf page 1: 25x25 modules of %.2f pt, top left at (%.1f, %.1f) pt from the page's "
        "top left\n" % (QR_M, QR_X, H - QR_TOP) + "\n".join(QR) + "\n"
    )


def make_jpeg():
    """A 190x95 picture (a mountain at dawn, our own) as a baseline JPEG, by macOS sips from a PNG we write."""
    out = os.path.join(HERE, "dawn.jpg")
    if os.path.exists(out):
        return open(out, "rb").read()
    w, h = 190, 95
    rows = b""
    for y in range(h):
        rows += b"\0"
        for x in range(w):
            peak = 70 - abs(x - 95) * 0.9
            if h - y < peak:
                r, g, b = 70 + y // 3, 60 + y // 4, 90
                if h - y > peak - 12 and abs(x - 95) < 30:
                    r, g, b = 240, 240, 245
            else:
                r, g, b = 250, 150 + y, 90 + y
            rows += bytes((min(r, 255), min(g, 255), min(b, 255)))
    png = b"\x89PNG\r\n\x1a\n" + chunk(
        b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    )
    png += chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")
    tmp = os.path.join(HERE, "dawn.png")
    open(tmp, "wb").write(png)
    subprocess.run(
        [
            "sips",
            "-s",
            "format",
            "jpeg",
            "-s",
            "formatOptions",
            "80",
            tmp,
            "--out",
            out,
        ],
        check=True,
        capture_output=True,
    )
    os.remove(tmp)
    return open(out, "rb").read()


def chunk(kind, data):
    return (
        struct.pack(">I", len(data))
        + kind
        + data
        + struct.pack(">I", zlib.crc32(kind + data))
    )


# ---------------------------------------------------------------- book-100.pdf

WORDS = [
    "the",
    "of",
    "and",
    "a",
    "to",
    "in",
    "is",
    "that",
    "it",
    "was",
    "for",
    "on",
    "are",
    "with",
    "as",
    "his",
    "they",
    "at",
    "be",
    "this",
    "from",
    "have",
    "or",
    "by",
    "one",
    "had",
    "not",
    "but",
    "what",
    "all",
    "were",
    "when",
    "we",
    "there",
    "can",
    "an",
    "your",
    "which",
    "their",
    "said",
    "if",
    "do",
    "will",
    "each",
    "about",
    "how",
    "up",
    "out",
    "them",
    "then",
    "she",
    "many",
    "some",
    "so",
    "these",
    "would",
    "other",
    "into",
    "has",
    "more",
    "her",
    "two",
    "like",
    "him",
    "see",
    "time",
    "could",
    "no",
    "make",
    "than",
    "first",
    "been",
    "its",
    "who",
    "now",
    "people",
    "my",
    "made",
    "over",
    "did",
    "down",
    "only",
    "way",
    "find",
    "use",
    "may",
    "water",
    "long",
    "little",
    "very",
    "after",
    "words",
    "called",
    "just",
    "where",
    "most",
    "know",
]


def book_100():
    p = Pdf()
    f1 = p.add(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
    )
    f2 = p.add(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
    )
    res = p.add("<< /Font << /F1 %d 0 R /F2 %d 0 R >> >>" % (f1, f2))
    pages = p.reserve()
    kids = []
    seed = 7
    for n in range(100):
        lines = []
        if n % 10 == 0:
            lines.append("BT /F2 22 Tf 72 740 Td (Chapter %d) Tj ET" % (n // 10 + 1))
        y = 700 if n % 10 == 0 else 740
        while y > 72:
            words = []
            while len(" ".join(words)) < 80:
                seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
                words.append(WORDS[seed % len(WORDS)])
            lines.append("BT /F1 11 Tf 72 %d Td (%s) Tj ET" % (y, " ".join(words)))
            y -= 15
        lines.append("BT /F1 9 Tf 290 40 Td (%d) Tj ET" % (n + 1))
        c = p.stream("", "\n".join(lines).encode())
        kids.append(
            p.add("<< /Type /Page /Parent %d 0 R /Contents %d 0 R >>" % (pages, c))
        )
    p.set(
        pages,
        "<< /Type /Pages /Kids [%s] /Count 100 /MediaBox [0 0 612 792] /Resources %d 0 R >>"
        % (" ".join("%d 0 R" % k for k in kids), res),
    )
    outlines = p.reserve()
    items = [p.reserve() for _ in range(10)]
    subs = [p.reserve() for _ in range(2)]
    for i, it in enumerate(items):
        title = "Գլուխ 3 · Chapter 3" if i == 2 else "Chapter %d" % (i + 1)
        links = "/Parent %d 0 R" % outlines
        if i > 0:
            links += " /Prev %d 0 R" % items[i - 1]
        if i < 9:
            links += " /Next %d 0 R" % items[i + 1]
        if i == 0:
            links += " /First %d 0 R /Last %d 0 R /Count 2" % (subs[0], subs[1])
        dest = (
            "/Dest [%d 0 R /XYZ 0 792 0]" % kids[i * 10]
            if i != 4
            else "/A << /S /GoTo /D [%d 0 R /Fit] >>" % kids[40]
        )
        p.set(it, "<< /Title %s %s %s >>" % (pdf_text(title), links, dest))
    for j, s in enumerate(subs):
        links = "/Parent %d 0 R" % items[0] + (
            " /Next %d 0 R" % subs[1] if j == 0 else " /Prev %d 0 R" % subs[0]
        )
        p.set(
            s,
            "<< /Title (Section 1.%d) %s /Dest [%d 0 R /Fit] >>"
            % (j + 1, links, kids[3 + j * 3]),
        )
    p.set(
        outlines,
        "<< /Type /Outlines /First %d 0 R /Last %d 0 R /Count 10 >>"
        % (items[0], items[-1]),
    )
    root = p.add(
        "<< /Type /Catalog /Pages %d 0 R /Outlines %d 0 R /PageMode /UseOutlines >>"
        % (pages, outlines)
    )
    info = p.add("<< /Title (A hundred pages) >>")
    p.save("book-100.pdf", root, info, objstm=True)


# ---------------------------------------------------------------- locked.pdf, hostile ones


def locked():
    p = Pdf()
    f = p.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    pages = p.reserve()
    c = p.stream(
        "",
        b"BT /F1 24 Tf 72 700 Td (The secret page) Tj ET\n0 0.5 0 rg 72 600 200 40 re f",
        flate=False,
    )
    page = p.add(
        "<< /Type /Page /Parent %d 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 %d 0 R >> >> "
        "/Contents %d 0 R >>" % (pages, f, c)
    )
    p.set(pages, "<< /Type /Pages /Kids [%d 0 R] /Count 1 >>" % page)
    root = p.add("<< /Type /Catalog /Pages %d 0 R >>" % pages)
    p.save("locked.pdf", root, password="1234")


def hostile():
    # Forms nested 200 deep, each drawing the next.
    p = Pdf()
    forms = [p.reserve() for _ in range(200)]
    for i, n in enumerate(forms):
        inner = "/X%d Do" % (i + 1) if i + 1 < len(forms) else "0 0 1 rg 0 0 10 10 re f"
        res = (
            "/Resources << /XObject << /X%d %d 0 R >> >>" % (i + 1, forms[i + 1])
            if i + 1 < len(forms)
            else ""
        )
        data = inner.encode()
        p.set(
            n,
            "<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] %s /Length %d >>"
            % (res, len(data)),
            data,
        )
    pages = p.reserve()
    c = p.stream("", b"/X0 Do 1 0 0 rg 0 0 50 50 re f")
    page = p.add(
        "<< /Type /Page /Parent %d 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /X0 %d 0 R >> >> "
        "/Contents %d 0 R >>" % (pages, forms[0], c)
    )
    p.set(pages, "<< /Type /Pages /Kids [%d 0 R] /Count 1 >>" % page)
    root = p.add("<< /Type /Catalog /Pages %d 0 R >>" % pages)
    p.save("deep.pdf", root)
    # A page tree whose second kid is itself, beside a page with a huge MediaBox.
    p = Pdf()
    pages = p.reserve()
    loop = p.reserve()
    c = p.stream("", b"1 0 0 rg 0 0 50 50 re f")
    good = p.add(
        "<< /Type /Page /Parent %d 0 R /MediaBox [0 0 99999999 99999999] /Contents %d 0 R >>" % (pages, c)
    )
    p.set(loop, "<< /Type /Pages /Parent %d 0 R /Kids [%d 0 R] /Count 1 >>" % (pages, loop))
    p.set(pages, "<< /Type /Pages /Kids [%d 0 R %d 0 R] /Count 2 >>" % (good, loop))
    root = p.add("<< /Type /Catalog /Pages %d 0 R >>" % pages)
    p.save("loop.pdf", root)
    # Every cross-reference offset wrong: readers must find the objects by scanning.
    q = Pdf()
    pages = q.reserve()
    c = q.stream("", b"0 0 1 rg 100 100 200 200 re f")
    page = q.add(
        "<< /Type /Page /Parent %d 0 R /MediaBox [0 0 400 400] /Contents %d 0 R >>"
        % (pages, c)
    )
    q.set(pages, "<< /Type /Pages /Kids [%d 0 R] /Count 1 >>" % page)
    root = q.add("<< /Type /Catalog /Pages %d 0 R >>" % pages)
    data = bytearray(q.save("broken-xref.pdf", root))
    x = data.rindex(b"xref\n")
    body = [bytes(line) for line in data[x:].split(b"\n")]
    for i, line in enumerate(body):
        if line.endswith(b" 00000 n "):
            body[i] = b"%010d 00000 n " % (int(line[:10]) + 7)
    data[x:] = b"\n".join(body)
    open(os.path.join(HERE, "broken-xref.pdf"), "wb").write(bytes(data))


def quartz():
    """macOS's own PDF writer (cupsfilter's text filter), for an outside view of fonts and structure."""
    out = os.path.join(HERE, "quartz.pdf")
    if os.path.exists(out) or sys.platform != "darwin":
        return
    txt = os.path.join(HERE, "quartz.txt")
    open(txt, "w").write(
        "Ծրագիր 2026\n\nThis page was written by macOS (Quartz) for the Files tests.\n"
        "Привет из Еревана.\n"
    )
    pdf = subprocess.run(
        ["cupsfilter", "-m", "application/pdf", txt], check=True, capture_output=True
    ).stdout
    open(out, "wb").write(pdf)
    os.remove(txt)


if __name__ == "__main__":
    boarding_pass()
    book_100()
    locked()
    hostile()
    quartz()
