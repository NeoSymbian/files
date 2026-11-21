# SPDX-License-Identifier: Apache-2.0
"""Writes the FA10 fixtures (our own content: the canvas's files2-view-docx board, Ծրագիր 2026) as .docx, .odt and
.rtf, with a heading, a subtitle, a list, a table, a picture and a page break. Run: python3 -I make.py"""
import os, struct, zipfile, zlib

HERE = os.path.dirname(os.path.abspath(__file__))
TITLE = "Ծրագիր 2026"
SUB = "Նախագիծ · 30 հոկտեմբերի 2026"
H1 = "1. Նպատակներ"
BODY = ("NeoSymbian-ը պետք է աշխատի Pixel 3a XL հեռախոսի վրա որպես ամենօրյա սարք։ "
        "Առաջին քայլը զանգերն ու հաղորդագրություններն են։")
ITEMS = ["Զանգեր և SMS", "Wi-Fi և դիտարկիչ", "Ֆայլեր և երաժշտություն"]
H2 = "2. Ժամանակացույց"
TABLE = [["Ամիս", "Աշխատանք"], ["Դեկտեմբեր", "Bluetooth"], ["Հունվար", "Նոր դիտարկիչ"]]
AFTER = "Երկրորդ էջ: the plan goes on after a page break, with a picture below."


def png(w=64, h=32):
    raw = b"".join(b"\0" + bytes(v for x in range(w) for v in (x * 4, y * 8, 160)) for y in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xffffffff)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def docx(path):
    W = 'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" ' \
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" ' \
        'xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" ' \
        'xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"'
    p = lambda text, ppr="", rpr="": f'<w:p><w:pPr>{ppr}</w:pPr><w:r><w:rPr>{rpr}</w:rPr><w:t xml:space="preserve">{esc(text)}</w:t></w:r></w:p>'
    cell = lambda t, bold: f'<w:tc><w:tcPr><w:tcW w:w="2400" w:type="dxa"/></w:tcPr>{p(t, rpr="<w:b/>" if bold else "")}</w:tc>'
    rows = "".join("<w:tr>" + "".join(cell(t, i == 0) for t in row) + "</w:tr>" for i, row in enumerate(TABLE))
    body = (p(TITLE, '<w:pStyle w:val="Title"/>')
            + p(SUB, rpr='<w:color w:val="6B6F76"/><w:sz w:val="20"/>')
            + p(H1, '<w:pStyle w:val="Heading2"/>')
            + '<w:p><w:r><w:t xml:space="preserve">NeoSymbian-ը պետք է աշխատի </w:t></w:r>'
              '<w:r><w:rPr><w:b/></w:rPr><w:t>Pixel 3a XL</w:t></w:r>'
              '<w:r><w:t xml:space="preserve"> հեռախոսի վրա որպես </w:t></w:r>'
              '<w:r><w:rPr><w:i/></w:rPr><w:t>ամենօրյա</w:t></w:r>'
              '<w:r><w:t xml:space="preserve"> սարք։ Առաջին քայլը զանգերն ու հաղորդագրություններն են։</w:t></w:r>'
              '<w:del><w:r><w:delText>deleted words</w:delText></w:r></w:del></w:p>'
            + "".join(p(t, '<w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr>') for t in ITEMS)
            + p(H2, '<w:pStyle w:val="Heading2"/>')
            + f'<w:tbl><w:tblGrid><w:gridCol w:w="2400"/><w:gridCol w:w="4800"/></w:tblGrid>{rows}</w:tbl>'
            + '<w:p><w:r><w:br w:type="page"/></w:r></w:p>'
            + p(AFTER)
            + '<w:p><w:r><w:drawing><wp:inline><wp:extent cx="2438400" cy="1219200"/><a:graphic><a:graphicData>'
              '<pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:blipFill>'
              '<a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>'
            + '<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="850" w:bottom="1134" w:left="1701"/></w:sectPr>')
    document = f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W}><w:body>{body}</w:body></w:document>'
    styles = (f'<?xml version="1.0" encoding="UTF-8"?><w:styles {W}>'
              '<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>'
              '<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>'
              '<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:basedOn w:val="Normal"/>'
              '<w:rPr><w:b/><w:sz w:val="52"/></w:rPr></w:style>'
              '<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/>'
              '<w:pPr><w:outlineLvl w:val="1"/></w:pPr><w:rPr><w:b/><w:color w:val="1F4E8C"/><w:sz w:val="36"/></w:rPr></w:style>'
              '</w:styles>')
    numbering = (f'<?xml version="1.0" encoding="UTF-8"?><w:numbering {W}><w:abstractNum w:abstractNumId="0">'
                 '<w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="\u2022"/></w:lvl>'
                 '</w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>')
    rels = ('<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>'
            '<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>'
            '<Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/>'
            '<Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.org" TargetMode="External"/>'
            '</Relationships>')
    types = ('<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
             '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
             '<Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/>'
             '<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
             '</Types>')
    root_rels = ('<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
                 '<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>'
                 '</Relationships>')
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", types)
        z.writestr("_rels/.rels", root_rels)
        z.writestr("word/document.xml", document)
        z.writestr("word/styles.xml", styles)
        z.writestr("word/numbering.xml", numbering)
        z.writestr("word/_rels/document.xml.rels", rels)
        z.writestr("word/media/image1.png", png())


def odt(path):
    NS = ('xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
          'xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" '
          'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
          'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" '
          'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" '
          'xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" '
          'xmlns:xlink="http://www.w3.org/1999/xlink" '
          'xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0"')
    auto = ('<office:automatic-styles>'
            '<style:style style:name="Sub" style:family="paragraph"><style:text-properties fo:color="#6b6f76" fo:font-size="10pt"/></style:style>'
            '<style:style style:name="B" style:family="text"><style:text-properties fo:font-weight="bold"/></style:style>'
            '<style:style style:name="I" style:family="text"><style:text-properties fo:font-style="italic"/></style:style>'
            '<style:style style:name="Brk" style:family="paragraph"><style:paragraph-properties fo:break-before="page"/></style:style>'
            '<text:list-style style:name="L1"><text:list-level-style-bullet text:level="1" text:bullet-char="•"/></text:list-style>'
            '</office:automatic-styles>')
    cells = lambda row: "".join(f'<table:table-cell><text:p>{esc(t)}</text:p></table:table-cell>' for t in row)
    body = (f'<text:h text:outline-level="1">{esc(TITLE)}</text:h>'
            f'<text:p text:style-name="Sub">{esc(SUB)}</text:p>'
            f'<text:h text:outline-level="2">{esc(H1)}</text:h>'
            '<text:p>NeoSymbian-ը պետք է աշխատի <text:span text:style-name="B">Pixel 3a XL</text:span> հեռախոսի վրա որպես '
            '<text:span text:style-name="I">ամենօրյա</text:span> սարք։ Առաջին քայլը զանգերն ու հաղորդագրություններն են։</text:p>'
            '<text:list text:style-name="L1">' + "".join(f'<text:list-item><text:p>{esc(t)}</text:p></text:list-item>' for t in ITEMS) + '</text:list>'
            f'<text:h text:outline-level="2">{esc(H2)}</text:h>'
            '<table:table><table:table-column table:number-columns-repeated="2"/>'
            + "".join(f'<table:table-row>{cells(r)}</table:table-row>' for r in TABLE) + '</table:table>'
            f'<text:p text:style-name="Brk">{esc(AFTER)}</text:p>'
            '<text:p><draw:frame svg:width="6.77cm" svg:height="3.39cm"><draw:image xlink:href="Pictures/image1.png"/></draw:frame></text:p>')
    content = f'<?xml version="1.0" encoding="UTF-8"?><office:document-content {NS}>{auto}<office:body><office:text>{body}</office:text></office:body></office:document-content>'
    styles = (f'<?xml version="1.0" encoding="UTF-8"?><office:document-styles {NS}><office:styles>'
              '<style:default-style style:family="paragraph"><style:text-properties fo:font-size="11pt"/></style:default-style>'
              '</office:styles><office:automatic-styles><style:page-layout style:name="pm1">'
              '<style:page-layout-properties fo:page-width="21cm" fo:page-height="29.7cm" fo:margin-top="2cm" '
              'fo:margin-bottom="2cm" fo:margin-left="3cm" fo:margin-right="1.5cm"/></style:page-layout>'
              '</office:automatic-styles></office:document-styles>')
    with zipfile.ZipFile(path, "w") as z:
        z.writestr(zipfile.ZipInfo("mimetype"), "application/vnd.oasis.opendocument.text")
        z.writestr("content.xml", content, zipfile.ZIP_DEFLATED)
        z.writestr("styles.xml", styles, zipfile.ZIP_DEFLATED)
        z.writestr("Pictures/image1.png", png())
        z.writestr("META-INF/manifest.xml", '<?xml version="1.0"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"/>')


def rtf(path):
    u = lambda s: "".join(c if ord(c) < 128 else f"\\u{ord(c) if ord(c) < 32768 else ord(c) - 65536}?" for c in s)
    rows = ""
    for i, row in enumerate(TABLE):
        open_ = "{\\b " if i == 0 else "{"
        rows += "\\trowd\\cellx2400\\cellx7200" + "".join("\\intbl " + open_ + u(t) + "}\\cell " for t in row) + "\\row\n"
    cp1251 = "Пример в cp1251".encode("cp1251")
    body = (f"{{\\pard\\qc\\fs52\\b {u(TITLE)}\\par}}\n"
            f"{{\\pard\\fs20\\cf1 {u(SUB)}\\par}}\n"
            f"{{\\pard\\fs36\\b\\cf2 {u(H1)}\\par}}\n"
            f"\\pard {u('NeoSymbian-ը պետք է աշխատի ')}{{\\b Pixel 3a XL}}{u(' հեռախոսի վրա որպես ')}{{\\i {u('ամենօրյա')}}}"
            f"{u(' սարք։ Առաջին քայլը զանգերն ու հաղորդագրություններն են։')}\\par\n"
            + "".join(f"{{\\pntext\\bullet\\tab}}{u(t)}\\par\n" for t in ITEMS)
            + f"{{\\pard\\fs36\\b\\cf2 {u(H2)}\\par}}\n" + rows
            + "\\pard " + "".join(f"\\'{b:02x}" for b in cp1251) + "\\par\\page\n"
            + f"{u(AFTER)}\\par\n"
            + "{\\pict\\pngblip\\picwgoal3840\\pichgoal1920 " + png().hex() + "}\\par\n")
    head = ("{\\rtf1\\ansi\\ansicpg1251\\deff0{\\fonttbl{\\f0\\fcharset204 Arial;}}"
            "{\\colortbl;\\red107\\green111\\blue118;\\red31\\green78\\blue140;}"
            "{\\*\\generator make.py;}{\\info{\\title Plan}}\\paperw11906\\paperh16838\\margl1701\\margr850\\margt1134\\margb1134\n")
    with open(path, "w", encoding="ascii") as f:
        f.write(head + body + "}")


if __name__ == "__main__":
    docx(os.path.join(HERE, "plan-hy.docx"))
    odt(os.path.join(HERE, "plan-hy.odt"))
    rtf(os.path.join(HERE, "plan-hy.rtf"))
