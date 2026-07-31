"""Cron helpers for recurring tasks (Фаза 3b §2, §4).

Everything schedule-related that does not touch the database lives here:
cron validation, next-run computation in a task's timezone, quiet-hours checks,
a human-readable description, and a deterministic natural-language → cron
parser ("каждый день в 8 вечера" → ``0 20 * * *``).

The natural-language parser is intentionally rule-based (no LLM call): the
agent-facing ``parse_cron`` tool has to be cheap, offline-safe and predictable.
Anything it cannot map returns ``None`` so the caller can fall back to asking
the user for an explicit cron expression.
"""

from __future__ import annotations

import re
from datetime import UTC, datetime, time
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from croniter import croniter

# Default time-of-day used when a natural-language phrase names a day but no
# hour ("каждый понедельник" → 09:00 that day).
DEFAULT_HOUR = 9
DEFAULT_MINUTE = 0

_WEEKDAY_NAMES: dict[str, int] = {
    # Monday = 1 in cron's 0-6 (Sunday = 0) numbering used here.
    "понедельник": 1,
    "вторник": 2,
    "серед": 3,  # среда / среду
    "четверг": 4,
    "пятниц": 5,
    "субботу": 6,
    "суббот": 6,
    "воскресень": 0,
    "monday": 1,
    "tuesday": 2,
    "wednesday": 3,
    "thursday": 4,
    "friday": 5,
    "saturday": 6,
    "sunday": 0,
    "mon": 1,
    "tue": 2,
    "wed": 3,
    "thu": 4,
    "fri": 5,
    "sat": 6,
    "sun": 0,
}

# "среда" needs its own key because the stem above ("серед") only matches the
# accusative form used in "каждую среду".
_WEEKDAY_NAMES["сред"] = 3


def resolve_timezone(name: str | None) -> ZoneInfo:
    """Return a ZoneInfo for *name*, falling back to UTC on unknown zones."""
    if not name:
        return ZoneInfo("UTC")
    try:
        return ZoneInfo(name)
    except (ZoneInfoNotFoundError, ValueError):
        return ZoneInfo("UTC")


def validate_cron(expression: str) -> str:
    """Validate and normalize a cron expression.

    Accepts the standard 5-field form (and the 6-field form with seconds that
    croniter understands). Returns the whitespace-normalized expression and
    raises ``ValueError`` when it is not parseable.
    """
    expr = " ".join((expression or "").split())
    if not expr:
        raise ValueError("Cron expression is empty")
    field_count = len(expr.split(" "))
    if field_count not in (5, 6):
        raise ValueError(
            f"Cron expression must have 5 fields (got {field_count}): "
            "minute hour day-of-month month day-of-week"
        )
    if not croniter.is_valid(expr):
        raise ValueError(f"Invalid cron expression: {expression!r}")
    return expr


def is_valid_cron(expression: str) -> bool:
    """Non-raising variant of :func:`validate_cron`."""
    try:
        validate_cron(expression)
    except ValueError:
        return False
    return True


def as_utc(moment: datetime) -> datetime:
    """Normalize a datetime to an aware UTC value (naive input = UTC)."""
    if moment.tzinfo is None:
        return moment.replace(tzinfo=UTC)
    return moment.astimezone(UTC)


def next_cron_run(
    expression: str,
    *,
    timezone: str | None = None,
    after: datetime | None = None,
) -> datetime:
    """Next fire time for *expression*, interpreted in *timezone*, as UTC.

    Raises ``ValueError`` for an invalid expression.
    """
    expr = validate_cron(expression)
    tz = resolve_timezone(timezone)
    base = as_utc(after or datetime.now(UTC)).astimezone(tz)
    nxt = croniter(expr, base).get_next(datetime)
    # croniter keeps the tzinfo of the base datetime.
    return as_utc(nxt)


def next_cron_runs(
    expression: str,
    *,
    timezone: str | None = None,
    count: int = 5,
    after: datetime | None = None,
) -> list[datetime]:
    """The next *count* fire times (UTC) for a cron expression."""
    expr = validate_cron(expression)
    tz = resolve_timezone(timezone)
    base = as_utc(after or datetime.now(UTC)).astimezone(tz)
    itr = croniter(expr, base)
    return [as_utc(itr.get_next(datetime)) for _ in range(max(0, count))]


def _parse_hhmm(value: str | None) -> time | None:
    """Parse an "HH:MM" (or "HH") string into a ``time``. None on garbage."""
    if not value:
        return None
    match = re.fullmatch(r"\s*(\d{1,2})(?::(\d{2}))?\s*", value)
    if match is None:
        return None
    hour = int(match.group(1))
    minute = int(match.group(2) or 0)
    if not (0 <= hour <= 23 and 0 <= minute <= 59):
        return None
    return time(hour=hour, minute=minute)


def in_quiet_hours(
    moment: datetime,
    *,
    timezone: str | None,
    start: str | None,
    end: str | None,
) -> bool:
    """Whether *moment* falls inside the task's quiet-hours window.

    The window is expressed in the task's local timezone and may wrap midnight
    ("23:00" → "07:00"). Missing/invalid bounds mean "no quiet hours".
    """
    start_t = _parse_hhmm(start)
    end_t = _parse_hhmm(end)
    if start_t is None or end_t is None or start_t == end_t:
        return False
    local = as_utc(moment).astimezone(resolve_timezone(timezone)).time()
    if start_t < end_t:
        return start_t <= local < end_t
    # Wrapping window (e.g. 23:00 - 07:00).
    return local >= start_t or local < end_t


# --- Human-readable description -------------------------------------------

_DOW_LABELS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"]


def describe_cron(expression: str) -> str:
    """Best-effort human-readable summary of a 5-field cron expression.

    Covers the shapes the UI/agent produce; anything else falls back to the raw
    expression so the caller always has something to show.
    """
    if not is_valid_cron(expression):
        return expression
    fields = " ".join(expression.split()).split(" ")
    if len(fields) == 6:  # drop the seconds field for the description
        fields = fields[1:]
    minute, hour, dom, month, dow = fields

    def _at() -> str:
        if minute.isdigit() and hour.isdigit():
            return f"at {int(hour):02d}:{int(minute):02d}"
        return ""

    step_min = re.fullmatch(r"\*/(\d+)", minute)
    if step_min and hour == "*" and dom == "*" and month == "*" and dow == "*":
        return f"every {step_min.group(1)} minutes"
    step_hour = re.fullmatch(r"\*/(\d+)", hour)
    if step_hour and minute.isdigit() and dom == "*" and month == "*" and dow == "*":
        return f"every {step_hour.group(1)} hours at minute {int(minute)}"
    if minute.isdigit() and hour == "*" and dom == "*" and month == "*" and dow == "*":
        return f"hourly at minute {int(minute)}"

    when = _at() or f"on cron {expression}"
    if dow != "*" and dom == "*":
        if dow == "1-5":
            return f"every weekday {when}"
        if dow in ("0,6", "6,0"):
            return f"every weekend day {when}"
        labels = []
        for part in dow.split(","):
            if part.isdigit() and 0 <= int(part) <= 6:
                labels.append(_DOW_LABELS[int(part)])
        if labels:
            return f"every {', '.join(labels)} {when}"
    if dom.isdigit() and dow == "*":
        return f"monthly on day {int(dom)} {when}"
    if dom == "*" and dow == "*" and month == "*":
        return f"daily {when}"
    return expression


# --- Natural language → cron ----------------------------------------------


def _extract_time(text: str) -> tuple[int, int] | None:
    """Find a time-of-day in *text*. Returns (hour, minute) or None.

    Understands "в 7:30", "в 8 вечера", "at 9am", "at 21:00", "18.45".
    """
    # Explicit HH:MM (or HH.MM).
    match = re.search(r"\b(\d{1,2})[:.](\d{2})\s*(am|pm|утра|дня|вечера|ночи)?", text)
    if match:
        hour = int(match.group(1))
        minute = int(match.group(2))
        hour = _apply_meridiem(hour, match.group(3))
        if 0 <= hour <= 23 and 0 <= minute <= 59:
            return hour, minute
    # Bare hour with a marker: "в 8 вечера", "at 9 am", "at 9".
    match = re.search(r"(?:\bв\b|\bat\b|\bк\b)\s*(\d{1,2})\s*(am|pm|утра|дня|вечера|ночи)?", text)
    if match:
        hour = _apply_meridiem(int(match.group(1)), match.group(2))
        if 0 <= hour <= 23:
            return hour, 0
    # Marker without the preposition: "8 вечера", "9pm".
    match = re.search(r"\b(\d{1,2})\s*(am|pm|утра|дня|вечера|ночи)\b", text)
    if match:
        hour = _apply_meridiem(int(match.group(1)), match.group(2))
        if 0 <= hour <= 23:
            return hour, 0
    return None


def _apply_meridiem(hour: int, marker: str | None) -> int:
    """Convert a 12-hour reading into 24-hour form using a RU/EN marker."""
    if not marker:
        return hour
    marker = marker.lower()
    if marker in ("pm", "вечера", "дня"):
        return hour if hour >= 12 else hour + 12
    if marker in ("am", "утра", "ночи"):
        return 0 if hour == 12 else hour
    return hour


def _find_weekday(text: str) -> int | None:
    for stem, dow in _WEEKDAY_NAMES.items():
        if stem in text:
            return dow
    return None


def parse_natural_schedule(text: str) -> str | None:
    """Translate a natural-language schedule into a 5-field cron expression.

    Supports the common recurring phrasings in Russian and English. Returns
    ``None`` when the phrase cannot be mapped confidently.
    """
    if not text:
        return None
    low = " ".join(text.lower().split())

    # --- Sub-hour / hourly intervals ---
    match = re.search(r"(?:кажд\w*|every)\s*(\d+)\s*(?:минут\w*|min(?:ute)?s?)\b", low)
    if match:
        step = max(1, min(59, int(match.group(1))))
        return f"*/{step} * * * *"
    if re.search(r"(?:кажд\w*\s+минут\w*|every\s+minute)\b", low):
        return "* * * * *"
    match = re.search(r"(?:кажд\w*|every)\s*(\d+)\s*(?:час\w*|hours?)\b", low)
    if match:
        step = max(1, min(23, int(match.group(1))))
        return f"0 */{step} * * *"
    if re.search(r"(?:кажд\w*\s+час\b|every\s+hour|hourly|ежечасно)", low):
        return "0 * * * *"

    hhmm = _extract_time(low)
    hour, minute = hhmm if hhmm else (DEFAULT_HOUR, DEFAULT_MINUTE)

    # --- Weekday sets ---
    if re.search(r"(?:по\s+будн\w*|будн\w*|weekdays?|рабоч\w*\s+дн\w*)", low):
        return f"{minute} {hour} * * 1-5"
    if re.search(r"(?:по\s+выходн\w*|выходн\w*|weekends?)", low):
        return f"{minute} {hour} * * 0,6"

    # --- Specific weekday ---
    dow = _find_weekday(low)
    if dow is not None:
        return f"{minute} {hour} * * {dow}"

    # --- Monthly ---
    match = re.search(r"\b(\d{1,2})\s*(?:числа|-го|th|st|nd|rd)\b", low)  # noqa: RUF001
    if match and re.search(r"(?:кажд\w*\s+месяц\w*|monthly|month)", low):
        day = max(1, min(28, int(match.group(1))))
        return f"{minute} {hour} {day} * *"
    if re.search(r"(?:кажд\w*\s+месяц\w*|monthly|ежемесячно)", low):
        return f"{minute} {hour} 1 * *"

    # --- Weekly without a named day ---
    if re.search(r"(?:кажд\w*\s+недел\w*|weekly|еженедельно)", low):
        return f"{minute} {hour} * * 1"

    # --- Daily ---
    if re.search(r"(?:кажд\w*\s+д(?:ен|н)\w*|ежедневно|daily|every\s+day)", low):
        return f"{minute} {hour} * * *"

    # A bare time ("в 8 вечера") is unambiguous enough to mean "daily".
    if hhmm is not None:
        return f"{minute} {hour} * * *"

    return None
