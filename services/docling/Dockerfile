# Docling Service Dockerfile
# Multi-stage build for smaller final image

FROM python:3.11-slim as builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    && rm -rf /var/lib/apt/lists/*

# Copy and install requirements
COPY requirements.txt .
RUN pip install --no-cache-dir --prefix=/install -r requirements.txt

# Final stage
FROM python:3.11-slim

WORKDIR /app

# Copy installed packages from builder
COPY --from=builder /install /usr/local

# Create non-root user
RUN useradd -m -u 1000 docling
USER docling

# Create assets directory
RUN mkdir -p /home/docling/assets && chown docling:docling /home/docling/assets

# Copy application
COPY --chown=docling:docling app.py .

# Environment variables
ENV ASSETS_DIR=/home/docling/assets
ENV PORT=50060
ENV EMBEDDING_MODEL=paraphrase-multilingual-MiniLM-L12-v2
ENV TRANSFORMERS_CACHE=/home/docling/.cache
ENV HF_HOME=/home/docling/.cache

# Expose port
EXPOSE 50060

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://localhost:50060/health')"

# Run the service
CMD ["python", "app.py"]
