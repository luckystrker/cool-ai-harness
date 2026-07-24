"""Cost budget policy and evaluation (Фаза 1.5 §5).

The spec puts cost guards partly in ``app/security/``. This module is the
policy layer: it knows nothing about the database — it takes a window's spend
plus a ``BudgetConfig`` and decides the budget status (ok / alert / blocked).
The ``app/budgets/service.py`` layer is responsible for fetching spend and
persisting budgets; the executor consumes the resulting ``BudgetStatus``.

Mirrors the shape of ``app/security/capabilities.py``: a small dataclass config
plus a pure evaluation function.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import UTC, datetime
from enum import Enum
from typing import Literal

from app.core.config import get_settings


class BudgetStatus(str, Enum):
    """Outcome of evaluating spend against the budget config."""

    OK = "ok"
    ALERT = "alert"  # spend crossed the alert threshold (e.g. 80 %)
    BLOCKED = "blocked"  # spend reached/exceeded a limit and block_on_exceed


Window = Literal["daily", "weekly", "monthly"]


@dataclass
class BudgetConfig:
    """Per-user cost-budget policy."""

    daily_limit_usd: float | None = None
    weekly_limit_usd: float | None = None
    monthly_limit_usd: float | None = None
    alert_threshold_pct: float = 80.0
    block_on_exceed: bool = True
    override_until: datetime | None = None

    @classmethod
    def from_settings(cls) -> BudgetConfig:
        s = get_settings()
        return cls(
            alert_threshold_pct=s.budget_alert_threshold_pct,
            block_on_exceed=s.budget_block_on_exceed,
        )

    @classmethod
    def from_row(cls, row) -> BudgetConfig:  # row: app.models.Budget
        return cls(
            daily_limit_usd=row.daily_limit_usd,
            weekly_limit_usd=row.weekly_limit_usd,
            monthly_limit_usd=row.monthly_limit_usd,
            alert_threshold_pct=row.alert_threshold_pct,
            block_on_exceed=row.block_on_exceed,
            override_until=row.override_until,
        )

    def is_overridden(self, now: datetime | None = None) -> bool:
        """Whether a block is currently lifted by an active override."""
        if self.override_until is None:
            return False
        now = now or datetime.now(UTC)
        # SQLite returns naive datetimes; treat them as UTC for comparison.
        until = self.override_until
        if until.tzinfo is None:
            until = until.replace(tzinfo=UTC)
        return until > now


@dataclass
class WindowSpend:
    """Spend for one window plus its limit and derived percentage."""

    window: Window
    spend_usd: float
    limit_usd: float | None

    @property
    def pct(self) -> float:
        """Spend as a percentage of the limit (0.0 when no limit set)."""
        if not self.limit_usd:
            return 0.0
        return (self.spend_usd / self.limit_usd) * 100.0

    @property
    def exceeded(self) -> bool:
        return self.limit_usd is not None and self.spend_usd >= self.limit_usd

    @property
    def alerted(self) -> bool:
        return self.limit_usd is not None and self.pct >= self._alert_pct_threshold

    # Alert is governed by the most stringent window's threshold, which equals
    # the config's alert_threshold_pct (all windows share it today).
    @property
    def _alert_pct_threshold(self) -> float:
        # Injected by evaluate(); default 80.
        return getattr(self, "_alert_threshold", 80.0)


@dataclass
class BudgetEvaluation:
    """Full evaluation result the executor and UI consume."""

    status: BudgetStatus
    windows: dict[Window, WindowSpend]
    overridden: bool

    @property
    def blocked(self) -> bool:
        return self.status == BudgetStatus.BLOCKED

    def should_alert(self) -> bool:
        return self.status == BudgetStatus.ALERT


def evaluate_budget(
    spend: dict[Window, float],
    config: BudgetConfig,
    *,
    now: datetime | None = None,
) -> BudgetEvaluation:
    """Decide budget status from per-window spend and a config.

    - BLOCKED: any configured window is exceeded AND block_on_exceed AND no
      active override.
    - else ALERT: any configured window crossed its alert threshold.
    - else OK.
    """
    now = now or datetime.now(UTC)
    overridden = config.is_overridden(now)

    alert_pct = config.alert_threshold_pct
    windows: dict[Window, WindowSpend] = {}
    for w in ("daily", "weekly", "monthly"):
        limit = {
            "daily": config.daily_limit_usd,
            "weekly": config.weekly_limit_usd,
            "monthly": config.monthly_limit_usd,
        }[w]
        ws = WindowSpend(window=w, spend_usd=spend.get(w, 0.0), limit_usd=limit)
        # Attach the alert threshold for the property (kept off the dataclass
        # to avoid serializing an internal knob).
        ws._alert_threshold = alert_pct  # type: ignore[attr-defined]
        windows[w] = ws

    any_exceeded = any(ws.exceeded for ws in windows.values())
    any_alerted = any(ws.alerted for ws in windows.values())

    if any_exceeded and config.block_on_exceed and not overridden:
        status = BudgetStatus.BLOCKED
    elif any_alerted:
        status = BudgetStatus.ALERT
    else:
        status = BudgetStatus.OK

    return BudgetEvaluation(status=status, windows=windows, overridden=overridden)
