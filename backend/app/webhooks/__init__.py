"""Webhook Router (Фаза 3b §7).

Layout:
- ``service`` — endpoint CRUD, HMAC verification, event processing, replay
"""

from __future__ import annotations

from app.webhooks.service import (
    create_endpoint,
    delete_endpoint,
    get_endpoint,
    get_endpoint_by_hook_id,
    list_endpoints,
    list_events,
    process_event,
    replay_event,
    update_endpoint,
    verify_signature,
)

__all__ = [
    "create_endpoint",
    "delete_endpoint",
    "get_endpoint",
    "get_endpoint_by_hook_id",
    "list_endpoints",
    "list_events",
    "process_event",
    "replay_event",
    "update_endpoint",
    "verify_signature",
]
