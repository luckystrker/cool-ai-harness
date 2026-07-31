"""Result delivery for scheduled task runs (Фаза 3b §3).

A finished ``TaskRun`` is delivered to the channels configured on its task.
Only the channels that exist in this phase are implemented:

- ``ui``       — the run shows up unread in the Tasks inbox (always available).
- ``webhook``  — HTTP POST of the run payload to a user-supplied URL, guarded
                 by the same SSRF checks the network tools use.
- ``telegram`` / ``email`` — recorded as ``unsupported`` until Фаза 5 / §8 land,
                 so a task configured for them still keeps its UI copy.

Delivery is deduplicated per task: an output identical to the previously
delivered one is not pushed again (only the UI copy is kept), which keeps a
noisy "nothing changed" monitor from spamming notifications.
"""

from __future__ import annotations

import hashlib
from datetime import UTC, datetime
from typing import Any

from sqlmodel import Session

from app.core.config import get_settings
from app.core.logging import get_logger
from app.models.task import (
    CHANNEL_EMAIL,
    CHANNEL_TELEGRAM,
    CHANNEL_UI,
    CHANNEL_WEBHOOK,
    ScheduledTask,
    TaskRun,
)
from app.security.secrets import mask_secrets_in_value
from app.security.ssrf import check_url_safety

log = get_logger(__name__)

# Max characters of task output pushed to an external channel.
MAX_DELIVERED_CHARS = 8_000


def output_hash(text: str | None) -> str:
    """Stable hash of a run's output, used for delivery deduplication."""
    return hashlib.sha256((text or "").encode("utf-8")).hexdigest()


def _payload(task: ScheduledTask, run: TaskRun) -> dict[str, Any]:
    """Webhook body describing a finished run."""
    body = (run.output or "")[:MAX_DELIVERED_CHARS]
    return {
        "task_id": task.id,
        "task_name": task.name,
        "task_run_id": run.id,
        "status": run.status,
        "trigger_source": run.trigger_source,
        "output": body,
        "error": run.error,
        "usage": run.usage,
        "duration_ms": run.duration_ms,
        "finished_at": run.finished_at.isoformat() if run.finished_at else None,
    }


async def _deliver_webhook(task: ScheduledTask, run: TaskRun) -> str:
    """POST the run payload to the task's webhook URL. Returns a status string."""
    url = (task.delivery_config or {}).get("webhook_url")
    if not url:
        return "failed: no webhook_url configured"

    settings = get_settings()
    verdict = check_url_safety(
        url,
        allowed_domains=settings.network_allowed_domains or None,
        block_private_ips=settings.ssrf_block_private_ips,
    )
    if not verdict.safe:
        return f"blocked: {verdict.reason}"

    import httpx

    payload = mask_secrets_in_value(_payload(task, run), enabled=settings.mask_secrets)
    try:
        async with httpx.AsyncClient(timeout=15.0) as client:
            response = await client.post(url, json=payload)
        if response.status_code >= 400:
            return f"failed: HTTP {response.status_code}"
    except Exception as exc:
        return f"failed: {exc}"
    return "ok"


async def deliver_task_run(
    session: Session, task: ScheduledTask, run: TaskRun
) -> dict[str, str]:
    """Deliver a finished run to its task's channels.

    Writes ``delivery_status`` / ``delivered_at`` onto the run and updates the
    task's dedup hash. Never raises: a delivery failure is recorded, not fatal.
    """
    channels = list(task.delivery_channels or [CHANNEL_UI])
    if CHANNEL_UI not in channels:
        # The UI copy is always kept — it is the inbox record of the run.
        channels.append(CHANNEL_UI)

    digest = output_hash(run.output)
    duplicate = bool(run.output) and digest == task.last_delivery_hash

    status: dict[str, str] = {}
    for channel in channels:
        if channel == CHANNEL_UI:
            status[CHANNEL_UI] = "ok"
        elif duplicate:
            status[channel] = "skipped: duplicate result"
        elif channel == CHANNEL_WEBHOOK:
            status[channel] = await _deliver_webhook(task, run)
        elif channel in (CHANNEL_TELEGRAM, CHANNEL_EMAIL):
            status[channel] = "unsupported: channel not implemented yet"
        else:
            status[channel] = f"unsupported: unknown channel {channel!r}"

    run.delivery_status = status
    run.delivered_at = datetime.now(UTC)
    session.add(run)
    if not duplicate and run.output:
        task.last_delivery_hash = digest
        session.add(task)
    session.commit()
    log.info(
        "task.delivered",
        task_id=task.id,
        task_run_id=run.id,
        channels=list(status.keys()),
        duplicate=duplicate,
    )
    return status
