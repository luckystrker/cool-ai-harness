# Backend image — Cool AI Harness
FROM python:3.12-slim

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1

WORKDIR /app

# Install the backend package.
COPY pyproject.toml ./
COPY app ./app
COPY ../README.md /tmp/README.md || true

RUN pip install --upgrade pip && pip install -e ".[dev]" || pip install -e "."

EXPOSE 8000

# Run with uvicorn. In production use --workers N and a process manager.
CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
