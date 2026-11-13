"""The image's document libraries: a workbook, a document and a PDF made and read."""

import docx
import openpyxl
from cryptography.hazmat.primitives import hashes
from lxml import etree
from pypdf import PdfReader
from reportlab.pdfgen import canvas

wb = openpyxl.Workbook()
wb.active["A1"] = 1
wb.save("o.xlsx")
d = docx.Document()
d.add_paragraph("hello")
d.save("o.docx")
c = canvas.Canvas("o.pdf")
c.drawString(72, 720, "hello")
c.save()
assert len(PdfReader("o.pdf").pages) == 1
h = hashes.Hash(hashes.SHA256())
h.update(b"x")
print(h.finalize().hex()[:8], etree.fromstring("<a><b/></a>").tag)
