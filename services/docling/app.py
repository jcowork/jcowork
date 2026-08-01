"""
Docling Document Conversion & Embedding Service

Provides HTTP endpoints for:
- Converting PDF documents to structured Markdown (with tables and images)
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
import tempfile
from contextlib import asynccontextmanager
from pathlib import Path
from typing import List, Optional

from fastapi import FastAPI, UploadFile, File, HTTPException, Form
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
    pipeline_options = PdfPipelineOptions()
    pipeline_options.do_ocr = True  # Enable OCR to handle embedded fonts correctly
    pipeline_options.do_table_structure = True
    pipeline_options.generate_picture_images = True  # Extract embedded images from PDF
    pipeline_options.images_scale = 2.0  # Higher resolution images
    # Configure OCR for Chinese text
    from docling.datamodel.pipeline_options import OcrOptions, EasyOcrOptions
    pipeline_options.ocr_options = EasyOcrOptions(lang=['ch_sim', 'en'])
    
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
    Convert a PDF (or DOCX) document to structured Markdown.
    
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
    suffix = Path(file.filename or "document.pdf").suffix.lower()
    if suffix not in [".pdf", ".docx", ".doc"]:
        raise HTTPException(status_code=400, detail=f"Unsupported file type: {suffix}")
    
    with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as tmp:
        tmp.write(content)
        tmp_path = tmp.name
    
    try:
        # Convert document
        result: ConversionResult = doc_converter.convert(tmp_path)
        
        # Generate a unique hash for this document (for image storage)
        doc_hash = hashlib.md5(content).hexdigest()[:12]
        doc_assets_dir = ASSETS_DIR / doc_hash
        doc_assets_dir.mkdir(parents=True, exist_ok=True)
        
        # Export to markdown
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
        
        # Post-process: remove page numbers that Docling inserts into the markdown.
        # These appear as standalone lines (e.g., "4") or embedded in text (e.g., "像 4 火").
        # Strategy:
        #   1. Remove lines that contain ONLY a number (standalone page numbers)
        #   2. Remove numbers embedded between Chinese characters where they break up text
        #      Pattern: Chinese_char + space(s) + digit(s) + space(s) + Chinese_char
        #      This catches cases like "像 4 火" -> "像火", "变成 4 4 了" -> "变成了"
        #      But preserves legitimate uses like "第4课", "17 猫", "(1)" etc.
        
        # Step 1: Remove standalone number lines
        markdown = re.sub(r'^\s*\d+\s*$', '', markdown, flags=re.MULTILINE)
        
        # Step 2: Remove numbers embedded between Chinese characters
        # Match: CJK char + whitespace + one or more (digits + whitespace) + (CJK char or CJK punctuation)
        # This handles single page numbers ("像 4 火") and consecutive ones ("变成 4 4 了", "盼望着 4 4 4 ，")
        # But preserves legitimate uses like "第4课", "17 猫", "(1)" etc.
        markdown = re.sub(
            r'([\u4e00-\u9fff])\s+((?:\d+\s+)+)([\u4e00-\u9fff\u3001\u3002\uff0c\uff1b\uff1a\uff01\uff1f\u201c\u201d\u2018\u2019\u300a\u300b\uff08\uff09])',
            r'\1\3',
            markdown
        )
        
        # Clean up any resulting double/triple spaces from the removal
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
        # Clean up temp file
        Path(tmp_path).unlink(missing_ok=True)


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
    
    # Generate embeddings
    embeddings = embedding_model.encode(
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
