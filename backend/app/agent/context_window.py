"""Context window management — sliding-window truncation for LLM history.

Keeps the conversation within the model's context window by dropping the
oldest non-system messages when the estimated token count exceeds the budget.
Tool-call / tool-result pairs are treated atomically: we never orphan a
tool_call without its results or vice versa.

Token estimation uses a chars/4 heuristic (no external dependencies). This is
a rough approximation but sufficient for window management — the goal is to
avoid 400 errors and runaway costs, not exact billing.
"""

from __future__ import annotations

from app.core.logging import get_logger
from app.providers import Message, message_text

log = get_logger(__name__)

# Average characters per token (rough heuristic for English text + code).
_CHARS_PER_TOKEN = 4


def estimate_tokens(text: str | None) -> int:
    """Estimate token count from character length."""
    if not text:
        return 0
    return max(1, len(text) // _CHARS_PER_TOKEN)


def estimate_message_tokens(msg: Message) -> int:
    """Estimate tokens for a single message (content + tool overhead)."""
    tokens = estimate_tokens(message_text(msg.content))
    if isinstance(msg.content, list):
        tokens += 1_000 * sum(1 for part in msg.content if part.get("type") == "image")
    # Tool calls add overhead (name, arguments JSON, structure).
    if msg.tool_calls:
        for tc in msg.tool_calls:
            tokens += 20  # function name + structure overhead
            args = tc.get("arguments")
            if isinstance(args, dict):
                import json

                tokens += estimate_tokens(json.dumps(args, default=str))
            elif isinstance(args, str):
                tokens += estimate_tokens(args)
    # Tool result messages have a tool_call_id + name overhead.
    if msg.role == "tool":
        tokens += 10
    return tokens


def estimate_history_tokens(history: list[Message]) -> int:
    """Estimate total tokens for a message history."""
    return sum(estimate_message_tokens(m) for m in history)


def truncate_history(
    history: list[Message],
    *,
    max_tokens: int,
) -> list[Message]:
    """Truncate history to fit within max_tokens, preserving structure.

    Strategy:
    1. Always keep the system message (index 0 if role == "system").
    2. Always keep the most recent user message (the current turn's input).
    3. Drop oldest non-system messages first, but never split a tool-call
       group (an assistant message with tool_calls + its subsequent tool
       result messages).

    Returns a new list (does not mutate the input).
    """
    if not history:
        return history

    total = estimate_history_tokens(history)
    if total <= max_tokens:
        return history

    # Separate system message from the rest.
    system_msg: Message | None = None
    messages: list[Message] = []
    for msg in history:
        if msg.role == "system" and system_msg is None:
            system_msg = msg
        else:
            messages.append(msg)

    system_tokens = estimate_message_tokens(system_msg) if system_msg else 0
    budget = max_tokens - system_tokens

    if budget <= 0:
        # System prompt alone exceeds the window — nothing else fits.
        log.warning(
            "context_window.system_prompt_exceeds_budget",
            system_tokens=system_tokens,
            max_tokens=max_tokens,
        )
        return [system_msg] if system_msg else []

    # Group messages into "atomic units":
    # - An assistant message with tool_calls + all following tool messages
    #   form one group (they must stay together).
    # - A standalone user or assistant message is its own group.
    groups: list[list[Message]] = []
    i = 0
    while i < len(messages):
        msg = messages[i]
        if msg.role == "assistant" and msg.tool_calls:
            # Collect the assistant + all subsequent tool results.
            group = [msg]
            i += 1
            while i < len(messages) and messages[i].role == "tool":
                group.append(messages[i])
                i += 1
            groups.append(group)
        else:
            groups.append([msg])
            i += 1

    # Compute token cost per group.
    group_tokens = [estimate_history_tokens(g) for g in groups]

    # Drop groups from the front until we fit within budget.
    # Always keep at least the last group (most recent exchange).
    start_idx = 0
    current_total = sum(group_tokens)

    while current_total > budget and start_idx < len(groups) - 1:
        current_total -= group_tokens[start_idx]
        start_idx += 1

    # Reassemble.
    kept_messages: list[Message] = []
    for g in groups[start_idx:]:
        kept_messages.extend(g)

    result: list[Message] = []
    if system_msg:
        result.append(system_msg)
    result.extend(kept_messages)

    dropped = len(messages) - len(kept_messages)
    if dropped > 0:
        log.info(
            "context_window.truncated",
            dropped_messages=dropped,
            kept_messages=len(kept_messages),
            estimated_tokens=current_total + system_tokens,
            max_tokens=max_tokens,
        )

    return result


def compute_history_budget(
    context_window_tokens: int,
    reserve_ratio: float,
) -> int:
    """Compute the token budget for history from settings.

    ``reserve_ratio`` is the fraction of the context window allocated to
    history (the rest is reserved for the model's output).
    """
    return int(context_window_tokens * reserve_ratio)
