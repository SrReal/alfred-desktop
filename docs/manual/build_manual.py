"""Build/check AlfredPet's user manual. Python: reportlab, pypdf, Pillow.

    python docs/manual/build_manual.py
    python docs/manual/build_manual.py --check

Run from any directory. Output stays at output/pdf/AlfredPet-Manual-de-uso.pdf.
The check binds the editable content, screenshots, builder, application sources
and final PDF. It detects stale artifacts; it does not replace editorial review.
"""

from pathlib import Path
from hashlib import sha256
from html import escape
import argparse
import json
import os
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
CONTENT = ROOT / "docs/manual/manual.es.json"
OUTPUT = ROOT / "output/pdf/AlfredPet-Manual-de-uso.pdf"
RECORD = ROOT / "docs/manual/verification.json"


TEXT_SUFFIXES = {".rs", ".toml", ".lock", ".json", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".cs", ".ps1", ".md", ".yml", ".yaml", ".html", ".css", ".svg", ".cmd", ".bat", ".txt"}


def canonical_text(data):
    # Git checkouts may use LF or CRLF. Binary resources remain byte-exact.
    return data.replace(b"\r\n", b"\n")


def digest(path, text=False):
    data = path.read_bytes()
    return sha256(canonical_text(data) if text else data).hexdigest()


def application_hash(files=None):
    if files is None:
        files = subprocess.check_output(
            ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
            cwd=ROOT,
        ).decode().split("\0")
    selected = sorted(set(p for p in files if p and (
        p.startswith(("src/", "src-tauri/", "native/", "sidecars/", "assets/"))
        or p in ("package.json", "package-lock.json", "index.html", "vite.config.ts")
    )))
    h = sha256()
    for p in selected:
        data = (ROOT / p).read_bytes()
        if Path(p).suffix.lower() in TEXT_SUFFIXES:
            data = canonical_text(data)
        h.update(p.encode() + b"\0" + data + b"\0")
    return h.hexdigest()


def bindings(data, files=None):
    images = {data["cover_image"]}
    images.update(b["path"] for page in data["pages"] for b in page["blocks"] if b["type"] == "image")
    return {
        "applicationSha256": application_hash(files),
        "contentSha256": digest(CONTENT, text=True),
        "builderSha256": digest(Path(__file__), text=True),
        "images": {p: digest(ROOT / p) for p in sorted(images)},
    }


def verify_pdf(data):
    from pypdf import PdfReader
    pdf = PdfReader(OUTPUT)
    expected = len(data["pages"]) + 2
    assert len(pdf.pages) == expected, f"Layout overflow: {len(pdf.pages)} pages, expected {expected}"
    assert len(pdf.outline) == expected, "Missing PDF bookmarks"
    for index, page in enumerate(data["pages"], 2):
        text = pdf.pages[index].extract_text()
        compact = lambda value: re.sub(r"\s+", "", value)
        assert compact(page["title"]) in compact(text), f"Wrong heading on page {index + 1}"
        assert "\ufffd" not in text and "\u25a0" not in text, f"Invalid glyph on page {index + 1}"
    ids = [p["id"] for p in data["pages"]]
    assert len(ids) == len(set(ids)), "Repeated chapter ID"
    catalog = (ROOT / "src-tauri/src/catalog.rs").read_text(encoding="utf-8")
    modules = re.findall(r'\bid: "([a-z_]+)"', catalog)
    assert sorted(modules) == sorted(data["covered_modules"]), "Manual module coverage is stale"
    return {"pages": expected, "bookmarks": len(pdf.outline), "modules": len(modules)}


def build(data):
    from reportlab.lib import colors
    from reportlab.lib.pagesizes import A4
    from reportlab.lib.styles import ParagraphStyle
    from reportlab.pdfbase import pdfmetrics
    from reportlab.pdfbase.ttfonts import TTFont
    from reportlab.platypus import (
        BaseDocTemplate, Frame, PageTemplate, Paragraph, Spacer, PageBreak,
        Table, TableStyle, Image, Flowable, KeepTogether,
    )

    if data.get("edition_kind") == "public":
        # Standard PDF fonts: the public guide embeds no Windows font files.
        from reportlab.pdfbase.pdfmetrics import Font
        for name, face in [("Body", "Helvetica"), ("Strong", "Helvetica-Bold"), ("Mono", "Courier")]:
            pdfmetrics.registerFont(Font(name, face, "WinAnsiEncoding"))
    else:
        font_dir = Path(os.environ.get("WINDIR", "C:/Windows")) / "Fonts"
        for name, filename in [("Body", "segoeui.ttf"), ("Strong", "segoeuib.ttf"), ("Mono", "consola.ttf")]:
            pdfmetrics.registerFont(TTFont(name, str(font_dir / filename)))
    pdfmetrics.registerFontFamily("Body", normal="Body", bold="Strong", italic="Body", boldItalic="Strong")
    pdfmetrics.registerFontFamily("Mono", normal="Mono", bold="Mono", italic="Mono", boldItalic="Mono")
    ink = colors.HexColor("#152933")
    teal = colors.HexColor("#007E92")
    muted = colors.HexColor("#516771")
    pale = colors.HexColor("#EDF5F6")
    width, height = A4
    margin = 48
    usable = width - margin * 2
    styles = {
        "body": ParagraphStyle("body", fontName="Body", fontSize=10.2, leading=14.2, textColor=ink, spaceAfter=8),
        "h": ParagraphStyle("h", fontName="Strong", fontSize=12.2, leading=16, textColor=teal, spaceBefore=9, spaceAfter=6, keepWithNext=True),
        "small": ParagraphStyle("small", fontName="Body", fontSize=8.5, leading=11.5, textColor=muted, spaceAfter=7),
        "title": ParagraphStyle("title", fontName="Strong", fontSize=24, leading=28, textColor=ink, spaceAfter=9),
        "sub": ParagraphStyle("sub", fontName="Body", fontSize=10, leading=14, textColor=muted, spaceAfter=16),
        "cell": ParagraphStyle("cell", fontName="Body", fontSize=9, leading=12.2, textColor=ink),
        "example": ParagraphStyle("example", fontName="Body", fontSize=10.1, leading=14.4, textColor=ink),
    }

    def para(text, style="body"):
        text = escape(text)
        text = re.sub(r"\*\*(.+?)\*\*", r"<b>\1</b>", text)
        text = re.sub(r"`(.+?)`", r'<font name="Mono">\1</font>', text)
        text = re.sub(r"\[([^\]]+)\]\((https?://[^ )]+)\)", r'<link href="\2" color="#007E92">\1</link>', text)
        return Paragraph(text.replace("\n", "<br/>"), styles[style])

    class Bookmark(Flowable):
        def __init__(self, key, title):
            super().__init__()
            self.key, self.title = key, title
            self.width = self.height = 0
        def draw(self):
            self.canv.bookmarkPage(self.key)
            self.canv.addOutlineEntry(self.title, self.key, level=0)

    count = len(data["pages"]) + 2

    def page_background(canvas, doc):
        canvas.saveState()
        if doc.page == 1:
            canvas.setFillColor(ink)
            canvas.rect(0, 0, width, height, fill=1, stroke=0)
            canvas.setStrokeColor(colors.HexColor("#294B59"))
            for offset in range(0, 480, 60):
                canvas.line(width - 210 + offset / 3, 0, width, 350 - offset / 2)
            canvas.setFillColor(colors.HexColor("#43CADC"))
            canvas.rect(margin, height - 76, 54, 4, fill=1, stroke=0)
            canvas.setFont("Strong", 12)
            canvas.drawString(margin, height - 105, "ALFREDPET / GUÍA DE USO")
            canvas.setFillColor(colors.white)
            canvas.setFont("Strong", 42)
            canvas.drawString(margin, height - 194, "Su asistente.")
            canvas.drawString(margin, height - 247, "Su escritorio.")
            canvas.setFont("Body", 16)
            canvas.drawString(margin, height - 286, "Manual completo de AlfredPet")
            canvas.setFillColor(colors.HexColor("#B7D2DB"))
            canvas.setFont("Body", 11)
            canvas.drawString(margin, 180, "Windows 11 x64  |  Aplicación " + data["product_version"])
            canvas.drawString(margin, 158, "Edición " + data["edition"] + "  |  " + data["updated_display"])
            canvas.drawString(margin, 125, "Chat · memoria · automatización · skills · plugins")
            canvas.setFont("Body", 9)
            canvas.drawString(margin, 66, "Fuente editable y comprobación de vigencia incluidas en el proyecto.")
        else:
            canvas.setFillColor(teal)
            canvas.rect(0, height - 7, width, 7, fill=1, stroke=0)
            canvas.setFont("Strong", 8)
            canvas.setFillColor(muted)
            canvas.drawString(margin, height - 33, "ALFREDPET  /  MANUAL DE USO")
            canvas.setFont("Body", 8)
            canvas.drawRightString(width - margin, height - 33, data["updated_display"])
            canvas.setStrokeColor(colors.HexColor("#D6E3E7"))
            canvas.line(margin, 45, width - margin, 45)
            canvas.drawString(margin, 30, "Ed. " + data["edition"] + " · v" + data["product_version"])
            canvas.drawRightString(width - margin, 30, f"{doc.page} / {count}")
        canvas.restoreState()

    doc = BaseDocTemplate(str(OUTPUT), pagesize=A4, title="AlfredPet - Manual de uso completo", author="AlfredPet", subject="Manual del usuario en español", pageCompression=1)
    doc.addPageTemplates(PageTemplate(id="main", frames=[Frame(margin, 58, usable, height - 112, leftPadding=0, bottomPadding=0, rightPadding=0, topPadding=0)], onPage=page_background))
    story = [Bookmark("cover", "Portada"), Spacer(1, 310)]
    cover = Image(str(ROOT / data["cover_image"]), width=120, height=160, kind="proportional")
    cover.hAlign = "RIGHT"
    story += [cover, PageBreak(), Bookmark("contents", "Índice"), para("Guía de consulta", "title"), para("Empieza por la puesta en marcha o pulsa un capítulo. Los marcadores laterales del PDF también permiten navegar.", "sub")]
    toc_rows = []
    for index, page in enumerate(data["pages"], 1):
        toc_rows.append([
            para(f"{index:02d}", "cell"),
            Paragraph(f'<link href="#{page["id"]}" color="#152933">{escape(page["title"])}</link>', styles["cell"]),
            para(str(index + 2), "cell"),
        ])
    toc = Table(toc_rows, colWidths=[30, usable - 60, 30], hAlign="LEFT")
    toc.setStyle(TableStyle([("VALIGN", (0, 0), (-1, -1), "TOP"), ("LEFTPADDING", (0, 0), (-1, -1), 0), ("RIGHTPADDING", (0, 0), (-1, -1), 0), ("TOPPADDING", (0, 0), (-1, -1), 1), ("BOTTOMPADDING", (0, 0), (-1, -1), 1)]))
    story += [toc, Spacer(1, 12), para("Las capturas proceden de perfiles de prueba del producto. Los ejemplos se adaptan a tus carpetas y conexiones; no ejecutan acciones por sí solos.", "small")]

    for index, page in enumerate(data["pages"], 1):
        story += [PageBreak(), Bookmark(page["id"], f"{index:02d} · {page['title']}"), para(f"{index:02d} / {page['title']}", "title"), para(page["subtitle"], "sub")]
        for block in page["blocks"]:
            kind = block["type"]
            if kind in ("p", "h", "small"):
                story.append(para(block["text"], {"p": "body", "h": "h", "small": "small"}[kind]))
            elif kind in ("steps", "bullets"):
                for n, text in enumerate(block["items"], 1):
                    prefix = f"{n}. " if kind == "steps" else "• "
                    story.append(para(prefix + text))
            elif kind in ("note", "example"):
                label = block.get("label", "Ejemplo para el chat" if kind == "example" else "Ten en cuenta")
                table = Table([[para("**" + label + "**\n" + block["text"], "example")]], colWidths=[usable])
                table.setStyle(TableStyle([("BACKGROUND", (0, 0), (-1, -1), pale), ("BOX", (0, 0), (-1, -1), .5, colors.HexColor("#C6DEE3")), ("LEFTPADDING", (0, 0), (-1, -1), 12), ("RIGHTPADDING", (0, 0), (-1, -1), 12), ("TOPPADDING", (0, 0), (-1, -1), 10), ("BOTTOMPADDING", (0, 0), (-1, -1), 10)]))
                story += [KeepTogether(table), Spacer(1, 9)]
            elif kind == "table":
                rows = [[para("**" + str(v) + "**", "cell") for v in block["headers"]]]
                rows.extend([[para(str(v), "cell") for v in row] for row in block["rows"]])
                ratios = block.get("widths", [1] * len(block["headers"]))
                table = Table(rows, colWidths=[usable * v / sum(ratios) for v in ratios], repeatRows=1, hAlign="LEFT")
                table.setStyle(TableStyle([("BACKGROUND", (0, 0), (-1, 0), pale), ("ROWBACKGROUNDS", (0, 1), (-1, -1), [colors.white, colors.HexColor("#F7FAFB")]), ("VALIGN", (0, 0), (-1, -1), "TOP"), ("LINEBELOW", (0, 0), (-1, 0), 1, teal), ("LEFTPADDING", (0, 0), (-1, -1), 8), ("RIGHTPADDING", (0, 0), (-1, -1), 8), ("TOPPADDING", (0, 0), (-1, -1), 4 if block.get("compact") else 7), ("BOTTOMPADDING", (0, 0), (-1, -1), 4 if block.get("compact") else 7)]))
                story += [table, Spacer(1, 10)]
            elif kind == "image":
                from PIL import Image as PILImage
                with PILImage.open(ROOT / block["path"]) as source:
                    ratio = min(usable / source.width, block.get("height", 240) / source.height)
                    visual = Image(str(ROOT / block["path"]), source.width * ratio, source.height * ratio)
                visual.hAlign = "CENTER"
                story += [visual, Spacer(1, 6), para(block["caption"], "small")]
            else:
                raise ValueError("Unknown block: " + kind)
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    doc.build(story)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    data = json.loads(CONTENT.read_text(encoding="utf-8"))
    bound = bindings(data)
    if args.check:
        record = json.loads(RECORD.read_text(encoding="utf-8"))
        assert all(record[k] == v for k, v in bound.items()), "Manual stale: review source, rebuild PDF, and render it again"
        assert record["pdfSha256"] == digest(OUTPUT), "PDF differs from verified generation"
        assert record["status"] == "PASS" and record.get("visualReview", {}).get("pagesReviewed") == len(data["pages"]) + 2, "Visual review pending"
        result = verify_pdf(data)
        print(json.dumps({"status": "PASS", **result}, ensure_ascii=False))
    else:
        build(data)
        result = verify_pdf(data)
        RECORD.write_text(json.dumps({"status": "STRUCTURAL_PASS_VISUAL_REVIEW_REQUIRED", "updated": data["updated"], "edition": data["edition"], "codeReference": data["code_reference"], **result, **bound, "pdfSha256": digest(OUTPUT)}, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({"output": str(OUTPUT), **result}, ensure_ascii=False))


if __name__ == "__main__":
    main()
