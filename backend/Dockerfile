# Backend image — Cool AI Harness
FROM python:3.12-slim

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1

WORKDIR /app

# OCR engine used by ocr_extract. Playwright installs the remaining Chromium
# shared libraries in the next layer.
RUN apt-get update \
    && apt-get install -y --no-install-recommends tesseract-ocr \
    && rm -rf /var/lib/apt/lists/*

# Install the backend package and the browser runtime.
COPY pyproject.toml README.md ./
COPY app ./app
COPY alembic ./alembic
COPY alembic.ini ./

RUN pip install --upgrade pip \
    && pip install -e "." \
    && python -m playwright install --with-deps chromium

EXPOSE 8000

# Run with uvicorn. In production use --workers N and a process manager.
CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
