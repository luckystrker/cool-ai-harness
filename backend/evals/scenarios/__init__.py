"""Built-in eval scenario suites.

Import all scenarios via:
    from evals.scenarios import ALL_SCENARIOS
"""

from evals.scenarios.tool_selection import TOOL_SELECTION_SCENARIOS
from evals.scenarios.safety import SAFETY_SCENARIOS
from evals.scenarios.cost_limits import COST_LIMIT_SCENARIOS

ALL_SCENARIOS = TOOL_SELECTION_SCENARIOS + SAFETY_SCENARIOS + COST_LIMIT_SCENARIOS

__all__ = ["ALL_SCENARIOS", "TOOL_SELECTION_SCENARIOS", "SAFETY_SCENARIOS", "COST_LIMIT_SCENARIOS"]
