"""
Docling Document Conversion & Embedding Service

Provides HTTP endpoints for:
- Converting PDF / Word / PowerPoint documents to structured Markdown
  (with tables and images)
- Generating text embeddings using local sentence-transformers model

Usage:
    python app.py
    # or with uvicorn
    uvicorn app:app --host 0.0.0.0 --port 50060
"""

import io
import os
import re
import base64
import hashlib
import platform
import shutil
import subprocess
import tempfile
from contextlib import asynccontextmanager
from pathlib import Path
from typing import List, Optional

from fastapi import FastAPI, UploadFile, File, HTTPException, Form
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import JSONResponse, FileResponse
from fastapi.staticfiles import StaticFiles
from pydantic import BaseModel
import uvicorn

# Docling imports
from docling.document_converter import DocumentConverter, PdfFormatOption
from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.datamodel.document import ConversionResult

# Sentence transformers for embeddings
from sentence_transformers import SentenceTransformer
import numpy as np

# Configuration
EMBEDDING_MODEL = os.getenv("EMBEDDING_MODEL", "paraphrase-multilingual-MiniLM-L12-v2")
ASSETS_DIR = Path(os.getenv("ASSETS_DIR", "/tmp/docling_assets"))
PORT = int(os.getenv("PORT", "50060"))

# Global model instances (loaded on startup)
doc_converter: Optional[DocumentConverter] = None
embedding_model: Optional[SentenceTransformer] = None
embedding_dim: int = 384  # Default dimension, updated on model load


def get_embedding_dimension() -> int:
    """Get embedding dimension, compatible with both old and new sentence-transformers API."""
    if embedding_model is None:
        return embedding_dim
    # Try new API first, fall back to old API
    if hasattr(embedding_model, 'get_embedding_dimension'):
        return embedding_model.get_embedding_dimension()
    return embedding_model.get_sentence_embedding_dimension()


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Load models on startup using lifespan event handler."""
    global doc_converter, embedding_model, embedding_dim
    
    print("Loading Docling converter...")
    
    # Configure Docling pipeline
    # NOTE: OCR (do_ocr) is intentionally disabled — it slowed conversion
    # ~3.6x (161s vs 44s for a 202-page PDF) while producing no better text
    # for PDFs that already carry a text layer. Scanned/image-only PDFs will
    # need text extraction to be added separately if ever required.
    pipeline_options = PdfPipelineOptions()
    pipeline_options.do_ocr = False
    pipeline_options.do_table_structure = True
    pipeline_options.generate_picture_images = True  # Extract embedded images from PDF
    pipeline_options.images_scale = 2.0  # Higher resolution images
    
    # Use proper FormatOption objects instead of plain dicts
    # Only configure PDF for now; other formats use defaults
    doc_converter = DocumentConverter(
        format_options={
            InputFormat.PDF: PdfFormatOption(pipeline_options=pipeline_options),
        }
    )
    print("Docling converter loaded.")
    
    print(f"Loading embedding model: {EMBEDDING_MODEL}")
    embedding_model = SentenceTransformer(EMBEDDING_MODEL)
    embedding_dim = get_embedding_dimension()
    print(f"Embedding model loaded. Dimension: {embedding_dim}")
    
    # Create assets directory
    ASSETS_DIR.mkdir(parents=True, exist_ok=True)
    
    yield


# Initialize FastAPI app with lifespan
app = FastAPI(title="Docling Service", version="1.0.0", lifespan=lifespan)


class ConvertResponse(BaseModel):
    """Response from PDF conversion endpoint."""
    markdown: str
    tables: List[str]  # List of markdown tables
    images: List[dict]  # List of {path, alt_text, base64}
    metadata: dict


class EmbedRequest(BaseModel):
    """Request for embedding generation."""
    texts: List[str]


class EmbedResponse(BaseModel):
    """Response with embeddings."""
    embeddings: List[List[float]]
    dimension: int


# --- Legacy Office format conversion (.doc -> .docx, .ppt -> .pptx) ---

# OLE2 Compound File magic bytes (genuine Word 97-2003 binary)
_OLE_MAGIC = b"\xd0\xcf\x11\xe0"


def _find_soffice() -> Optional[str]:
    """Locate the LibreOffice headless binary, if installed."""
    soffice = shutil.which("soffice") or shutil.which("libreoffice")
    if soffice:
        return soffice
    if platform.system() == "Darwin":
        mac_path = "/Applications/LibreOffice.app/Contents/MacOS/soffice"
        if Path(mac_path).exists():
            return mac_path
    return None


def convert_legacy_doc_to_docx(src_path: Path) -> Optional[Path]:
    """Convert a legacy binary .doc file to .docx.

    Docling parses .docx natively but needs an external converter for the
    binary Word 97-2003 format. Tries LibreOffice first (cross-platform,
    best fidelity), then falls back to macOS's built-in ``textutil``.

    Returns the path of the converted .docx, or None when no converter is
    available or the conversion failed.
    """
    out_dir = Path(tempfile.mkdtemp(prefix="docling_doc_conv_"))
    out_path = out_dir / (src_path.stem + ".docx")

    soffice = _find_soffice()
    if soffice:
        try:
            subprocess.run(
                [soffice, "--headless", "--convert-to", "docx",
                 "--outdir", str(out_dir), str(src_path)],
                check=True,
                capture_output=True,
                timeout=120,
            )
            if out_path.exists():
                return out_path
        except (subprocess.SubprocessError, OSError) as e:
            print(f"LibreOffice .doc conversion failed: {e}")

    # Fallback: macOS built-in textutil (no extra install required)
    if platform.system() == "Darwin" and shutil.which("textutil"):
        try:
            subprocess.run(
                ["textutil", "-convert", "docx", str(src_path),
                 "-output", str(out_path)],
                check=True,
                capture_output=True,
                timeout=120,
            )
            if out_path.exists():
                return out_path
        except (subprocess.SubprocessError, OSError) as e:
            print(f"textutil .doc conversion failed: {e}")

    return None


def convert_legacy_ppt_to_pptx(src_path: Path) -> Optional[Path]:
    """Convert a legacy binary .ppt file (PowerPoint 97-2003) to .pptx.

    Docling reads .pptx natively through python-pptx but cannot open the
    binary format, and macOS textutil has no presentation support, so
    LibreOffice is the only converter. Returns the converted path, or None
    when LibreOffice is missing / the conversion failed.
    """
    soffice = _find_soffice()
    if soffice is None:
        return None

    out_dir = Path(tempfile.mkdtemp(prefix="docling_ppt_conv_"))
    out_path = out_dir / (src_path.stem + ".pptx")
    try:
        subprocess.run(
            [soffice, "--headless", "--convert-to", "pptx",
             "--outdir", str(out_dir), str(src_path)],
            check=True,
            capture_output=True,
            timeout=180,
        )
    except (subprocess.SubprocessError, OSError) as e:
        print(f"LibreOffice .ppt conversion failed: {e}")
        return None
    return out_path if out_path.exists() else None


def export_presentation_markdown(doc) -> str:
    """Export a slide deck with one `---` separated section per slide.

    Docling's generic Markdown export concatenates every slide into one
    running text, which makes decks hard to read. Grouping items by slide
    provenance restores the deck's page structure; pictures stay as
    ``<!-- image -->`` placeholders for the image pass of /convert.
    """
    parts: List[str] = []
    current_page: Optional[int] = None
    for item, _level in doc.iterate_items():
        prov = getattr(item, "prov", None)
        page = prov[0].page_no if prov else None
        if current_page is not None and page is not None and page != current_page:
            parts.append("---")
        if page is not None:
            current_page = page

        kind = type(item).__name__
        if kind == "TableItem":
            text = item.export_to_markdown(doc=doc)
        elif kind == "PictureItem":
            text = "<!-- image -->"
        else:
            text = (getattr(item, "text", "") or "").strip()
            if text and kind == "ListItem":
                text = f"- {text}"
        if text:
            parts.append(text)
    return "\n\n".join(parts)


@app.get("/health")
async def health():
    """Health check endpoint."""
    return {
        "status": "ok",
        "docling_loaded": doc_converter is not None,
        "embedding_loaded": embedding_model is not None,
        "embedding_model": EMBEDDING_MODEL,
        "embedding_dim": get_embedding_dimension(),
    }


@app.post("/convert", response_model=ConvertResponse)
async def convert_document(file: UploadFile = File(...)):
    """
    Convert a PDF, Word or PowerPoint document to structured Markdown.
    
    Returns:
    - markdown: Full markdown content
    - tables: List of extracted tables (as markdown)
    - images: List of extracted images with paths and metadata
    - metadata: Document metadata (page count, etc.)
    """
    if doc_converter is None:
        raise HTTPException(status_code=503, detail="Docling converter not loaded")
    
    # Read uploaded file
    content = await file.read()
    
    # Create a temporary file for docling to process
    original_suffix = Path(file.filename or "document.pdf").suffix.lower()
    suffix = original_suffix
    if suffix not in [".pdf", ".docx", ".doc", ".pptx", ".ppt"]:
        raise HTTPException(status_code=400, detail=f"Unsupported file type: {suffix}")
    
    # Legacy binary Office formats (Word 97-2003 .doc, PowerPoint 97-2003 .ppt)
    # are not readable by Docling's OOXML backends, so pre-convert them first.
    # Word can fall back to macOS textutil; PowerPoint needs LibreOffice.
    converted_path: Optional[Path] = None
    if suffix in (".doc", ".ppt") and content[:4] == _OLE_MAGIC:
        with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as tmp:
            tmp.write(content)
            legacy_path = Path(tmp.name)
        try:
            conversion = await run_in_threadpool(
                convert_legacy_ppt_to_pptx if suffix == ".ppt" else convert_legacy_doc_to_docx,
                legacy_path,
            )
        finally:
            legacy_path.unlink(missing_ok=True)
        if conversion is None:
            target = ".pptx" if suffix == ".ppt" else ".docx"
            raise HTTPException(
                status_code=501,
                detail=(
                    f"Legacy {suffix} conversion requires LibreOffice (not installed). "
                    f"Install LibreOffice, or save the file as {target} and re-upload."
                ),
            )
        converted_path = conversion
        tmp_path = str(conversion)
        suffix = ".pptx" if original_suffix == ".ppt" else ".docx"
    else:
        # A .ppt that is really OOXML (a renamed .pptx) must reach python-pptx
        # with the right suffix, otherwise Docling's format sniffing bails out.
        tmp_suffix = ".pptx" if suffix == ".ppt" else suffix
        with tempfile.NamedTemporaryFile(suffix=tmp_suffix, delete=False) as tmp:
            tmp.write(content)
            tmp_path = tmp.name
    
    try:
        # Convert document (blocking CPU work — run in threadpool so the
        # event loop stays responsive for /embed and /health requests)
        result: ConversionResult = await run_in_threadpool(doc_converter.convert, tmp_path)
        
        # Generate a unique hash for this document (for image storage)
        doc_hash = hashlib.md5(content).hexdigest()[:12]
        doc_assets_dir = ASSETS_DIR / doc_hash
        doc_assets_dir.mkdir(parents=True, exist_ok=True)
        
        # Export to markdown (presentations are grouped one section per slide)
        if original_suffix in (".pptx", ".ppt"):
            markdown = export_presentation_markdown(result.document)
        else:
            markdown = result.document.export_to_markdown()
        
        # Extract tables (pass doc for proper export to avoid deprecation warning)
        tables = []
        for table in result.document.tables:
            table_md = table.export_to_markdown(doc=result.document)
            tables.append(table_md)
        
        # Extract images and save to assets directory
        images = []
        for img_idx, img in enumerate(result.document.pictures):
            # Get image data - img.image is an ImageRef object
            # Access the actual PIL image via .pil_image attribute
            img_ref = img.image
            
            if img_ref is not None:
                # ImageRef has pil_image attribute containing the actual PIL Image
                pil_img = getattr(img_ref, 'pil_image', None)
                if pil_img is None:
                    continue
                
                # Generate filename
                img_filename = f"image_{img_idx:03d}.png"
                img_path = doc_assets_dir / img_filename
                
                # Save image
                pil_img.save(img_path)
                
                # Get page number from prov (it's a list of ProvenanceItem)
                page_no = 0
                if hasattr(img, 'prov') and img.prov:
                    page_no = img.prov[0].page_no if len(img.prov) > 0 else 0
                
                images.append({
                    "path": str(img_path),
                    "filename": img_filename,
                    "page": page_no,
                    "alt_text": f"Image {img_idx + 1}",
                    "index": img_idx,
                })
        
        # Post-process markdown: replace <!-- image --> placeholders with actual image references
        # Docling outputs "<!-- image -->" for each picture in order
        if images:
            img_counter = 0
            def replace_image_placeholder(match):
                nonlocal img_counter
                if img_counter < len(images):
                    img_info = images[img_counter]
                    img_counter += 1
                    return f"![{img_info['alt_text']}]({img_info['filename']})"
                return match.group(0)
            
            markdown = re.sub(r'<!--\s*image\s*-->', replace_image_placeholder, markdown, flags=re.IGNORECASE)
        
        # PDF page-noise cleanup. Docling sprinkles bare page numbers into PDF
        # markdown (standalone lines, or digits wedged between Chinese
        # characters). Decks and Word files must keep numbers that are real
        # content (e.g. a slide whose body is just "2024"), so PDFs only.
        if original_suffix == ".pdf":
            # Step 1: drop lines that contain ONLY a page number
            markdown = re.sub(r'^\s*\d+\s*$', '', markdown, flags=re.MULTILINE)

            # Step 2: drop numbers wedged between Chinese characters
            # ("像 4 火" -> "像火", "变成 4 4 了" -> "变成了") while keeping
            # legitimate uses like "第4课", "17 猫", "(1)".
            markdown = re.sub(
                r'([\u4e00-\u9fff])\s+((?:\d+\s+)+)([\u4e00-\u9fff\u3001\u3002\uff0c\uff1b\uff1a\uff01\uff1f\u201c\u201d\u2018\u2019\u300a\u300b\uff08\uff09])',
                r'\1\3',
                markdown
            )

            # Collapse the double/triple spaces the removals leave behind
            markdown = re.sub(r' {2,}', ' ', markdown)
        
        print(f"Extracted {len(images)} images from document, doc_hash={doc_hash}")
        
        # Build metadata
        metadata = {
            "page_count": len(result.document.pages),
            "doc_hash": doc_hash,
            "assets_dir": str(doc_assets_dir),
        }
        
        return ConvertResponse(
            markdown=markdown,
            tables=tables,
            images=images,
            metadata=metadata,
        )
        
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"Conversion failed: {str(e)}")
    
    finally:
        # Clean up temp file (and the converted .docx when a legacy .doc was
        # pre-converted; its parent dir holds only that file)
        Path(tmp_path).unlink(missing_ok=True)
        if converted_path is not None:
            shutil.rmtree(converted_path.parent, ignore_errors=True)


@app.post("/embed", response_model=EmbedResponse)
async def embed_texts(request: EmbedRequest):
    """
    Generate embeddings for a batch of texts.
    
    Returns vectors as float arrays (dimension depends on model).
    """
    if embedding_model is None:
        raise HTTPException(status_code=503, detail="Embedding model not loaded")
    
    if not request.texts:
        return EmbedResponse(embeddings=[], dimension=get_embedding_dimension())
    
    if len(request.texts) > 100:
        raise HTTPException(status_code=400, detail="Batch size limited to 100 texts")
    
    # Generate embeddings (blocking CPU work — run in threadpool so a
    # concurrent /convert does not starve embedding requests)
    embeddings = await run_in_threadpool(
        embedding_model.encode,
        request.texts,
        normalize_embeddings=True,  # L2 normalize for cosine similarity
        show_progress_bar=False,
    )
    
    # Convert to list of lists
    embeddings_list = embeddings.tolist()
    
    return EmbedResponse(
        embeddings=embeddings_list,
        dimension=get_embedding_dimension(),
    )


@app.get("/assets/{doc_hash}/{filename}")
async def get_asset(doc_hash: str, filename: str):
    """Serve an extracted image asset."""
    file_path = ASSETS_DIR / doc_hash / filename
    
    if not file_path.exists():
        raise HTTPException(status_code=404, detail="Asset not found")
    
    # Security: ensure path is within assets dir
    if not str(file_path.resolve()).startswith(str(ASSETS_DIR.resolve())):
        raise HTTPException(status_code=403, detail="Access denied")
    
    return FileResponse(file_path)


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=PORT)
