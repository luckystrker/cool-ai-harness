"""Subscription provider adapters (experimental).

These adapters provide experimental access to LLM services via consumer
subscription plans (Claude Pro/Max, ChatGPT Plus) rather than API keys.

WARNING: These are EXPERIMENTAL and may break at any time as they rely on
unofficial/reverse-engineered endpoints. They are NOT covered by any SLA
and may violate the service's Terms of Use. Use at your own risk.

Architecture:
- Each adapter implements the standard ``LLMProvider`` interface.
- Authentication uses session tokens/cookies extracted from the browser.
- Rate limits are governed by the subscription tier, not API quotas.
"""

from app.providers.subscription.chatgpt_plus import ChatGPTPlusProvider
from app.providers.subscription.claude_pro import ClaudeProProvider

__all__ = ["ChatGPTPlusProvider", "ClaudeProProvider"]
