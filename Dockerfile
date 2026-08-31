# syntax=docker/dockerfile:1.7

FROM node:22-bookworm-slim AS frontend-build
WORKDIR /src/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

FROM python:3.12-slim AS runtime

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PLAYWRIGHT_BROWSERS_PATH=/ms-playwright \
    PLAYWRIGHT_DOWNLOAD_CONNECTION_TIMEOUT=120000 \
    COOL_HOME=/var/lib/cool \
    COOL_CONFIG_FILE=/var/lib/cool/config.yaml \
    DATA_DIR=/var/lib/cool/data \
    WORKSPACES_DIR=/var/lib/cool/workspaces \
    SKILLS_DIR=/var/lib/cool/skills \
    ARTIFACTS_DIR=/var/lib/cool/data/artifacts \
    FRONTEND_DIST=/opt/cool/frontend/dist \
    DATABASE_URL=sqlite:////var/lib/cool/data/harness.db \
    ENVIRONMENT=production \
    DEBUG=false

WORKDIR /opt/cool/backend

RUN apt-get update \
    && apt-get install -y --no-install-recommends git tesseract-ocr \
    && rm -rf /var/lib/apt/lists/*

COPY backend/pyproject.toml backend/README.md ./
COPY backend/app ./app
COPY backend/alembic ./alembic
COPY backend/alembic.ini ./
COPY skills /opt/cool/skills

RUN pip install --upgrade pip \
    && pip install -e . \
    && python -m playwright install --with-deps chromium

COPY --from=frontend-build /src/frontend/dist /opt/cool/frontend/dist

RUN useradd --create-home --uid 10001 cool \
    && mkdir -p /var/lib/cool/data/artifacts /var/lib/cool/workspaces /var/lib/cool/skills \
    && chown -R cool:cool /var/lib/cool /opt/cool /ms-playwright

USER cool
VOLUME ["/var/lib/cool"]
EXPOSE 8000

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8000/api/health', timeout=3)"

CMD ["cool", "serve", "--host", "0.0.0.0", "--port", "8000"]
