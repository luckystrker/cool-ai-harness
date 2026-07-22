# Фаза 5: Telegram (Bot + Web App) + Voice Interface

> **Статус:** ⏳ Ожидает
> **Длительность:** 1.5 недели

Цель: полный контроль harness'а с телефона + голосовой интерфейс.

---

## 1. Telegram Bot (`app/telegram/bot/`)

- Через `python-telegram-bot` (async)
- Команды: `/start`, `/chat`, `/profile`, `/research`, `/dnd`, `/code`, `/tasks`, `/rss`, `/settings`
- Inline-кнопки (профили, модели, действия по задачам)
- Streaming через edit_message
- Уведомления о завершении long-running и recurring tasks

### Telegram + Breakpoints 🆕

- Inline-кнопки Approve / Reject / Edit args для HITL breakpoints (из Фазы 1.5)
- Уведомление о breakpoint с контекстом + кнопки действий

## 2. Telegram Web App (`app/telegram/webapp/`)

- React-приложение внутри Telegram, авторизация через `initData`
- Полный UI: чат, профили, memory, аналитика, tasks, RSS, webhooks
- Нативные фичи: камера (multimodal), файлы, геолокация
- Темизация под Telegram

## 3. Voice Interface 🆕

- **Voice Input**: голосовой ввод через Telegram voice messages → Whisper API (OpenAI / local)
- **Voice Output**: TTS для ответов (ElevenLabs, OpenAI TTS, или локальный)
- **Hands-free режим**: голосовой чат в Telegram как альтернатива тексту
- **Voice Transcription tool**: `transcribe_audio(audio_file) → text`
- **Voice Personality**: отдельный agent profile для голосового общения (более краткие, разговорные ответы)
- **Deep research voice report**: зачитать дайджест голосом (TTS)
- **Voice shortcuts**: ключевые фразы для быстрых действий ("запусти дайджест", "что нового?")

## 4. Общий auth

- Один пользователь, единый токен между web/telegram
- Telegram initData validation

## Деливерабл

Полный контроль harness'а с телефона, включая управление recurring tasks и RSS; голосовой ввод/вывод в Telegram.
