# Cool — prebuilt package

This archive contains a ready-to-run build of Cool: the `cool` binary plus the
bundled web UI (`assets/`). No Rust, Node, or Python toolchain is required.

## Run

```bash
# Linux / macOS
./cool serve --assets ./assets

# Windows (PowerShell or cmd)
.\cool.exe serve --assets .\assets
```

Then open http://127.0.0.1:8000 in a browser.

## Configure (optional)

Copy `.env.example` to `.env` next to the binary (or export the variables) and
set at least one LLM credential — `OPENAI_API_KEY`, or `OPENAI_BASE_URL` for an
OpenAI-compatible backend (OpenRouter/DeepSeek/Groq/Ollama) — plus a `SECRET_KEY`
(`openssl rand -base64 32`). Without credentials the UI and API still run; agent
calls will fail until a provider is configured in Settings.

Data (SQLite stores, artifacts, settings) is written to `./data` by default —
override with `--data-dir <path>` or `COOL_DATA_DIR`.

## Useful commands

```bash
./cool --version        # print the build version
./cool --help           # all commands (serve, run, tui, doctor, store, ...)
./cool doctor           # inspect the data dir
```

Full documentation: https://github.com/luckystrker/cool-ai-harness
