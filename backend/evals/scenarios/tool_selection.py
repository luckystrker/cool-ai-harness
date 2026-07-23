"""Tool selection scenarios: verify the agent picks the right tool for the task.

These scenarios test that given a user request, the scripted LLM's tool choice
is correctly executed by the agent loop (tool is called, result is returned,
the loop continues or finishes appropriately).
"""

from evals.scenario import EvalScenario, ScenarioAssertion, Severity

TOOL_SELECTION_SCENARIOS: list[EvalScenario] = [
    EvalScenario(
        id="tool_select_read_file",
        name="Read file selection",
        description="Agent calls read_file when user asks to read a file",
        tags=["tool_selection", "files"],
        severity=Severity.CRITICAL,
        input="Read the file hello.txt and show me its contents",
        script=[
            [{"name": "read_file", "arguments": {"path": "hello.txt"}}],
            "The file contains: Hello, World!",
        ],
        assertions=[
            ScenarioAssertion(type="tool_called", name="read_file"),
            ScenarioAssertion(
                type="tool_called", name="read_file", arguments={"path": "hello.txt"}
            ),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
    EvalScenario(
        id="tool_select_write_file",
        name="Write file selection",
        description="Agent calls write_file when user asks to create a file",
        tags=["tool_selection", "files"],
        severity=Severity.CRITICAL,
        input="Create a file called output.txt with the text 'done'",
        script=[
            [{"name": "write_file", "arguments": {"path": "output.txt", "content": "done"}}],
            "I've created output.txt with the content 'done'.",
        ],
        assertions=[
            ScenarioAssertion(type="tool_called", name="write_file"),
            ScenarioAssertion(
                type="tool_called",
                name="write_file",
                arguments={"path": "output.txt", "content": "done"},
            ),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
    EvalScenario(
        id="tool_select_list_files",
        name="List files selection",
        description="Agent calls list_files when user asks to see directory contents",
        tags=["tool_selection", "files"],
        severity=Severity.IMPORTANT,
        input="What files are in the current directory?",
        script=[
            [{"name": "list_files", "arguments": {"path": "."}}],
            "The directory contains: a.txt, b.txt, and subdir/.",
        ],
        assertions=[
            ScenarioAssertion(type="tool_called", name="list_files"),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
    EvalScenario(
        id="tool_select_python_execute",
        name="Python execution selection",
        description="Agent calls python_execute for computation tasks",
        tags=["tool_selection", "code"],
        severity=Severity.CRITICAL,
        input="Calculate the factorial of 10",
        script=[
            [
                {
                    "name": "python_execute",
                    "arguments": {"code": "import math; print(math.factorial(10))"},
                }
            ],
            "The factorial of 10 is 3,628,800.",
        ],
        assertions=[
            ScenarioAssertion(type="tool_called", name="python_execute"),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
    EvalScenario(
        id="tool_select_web_search",
        name="Web search selection",
        description="Agent calls web_search when user asks for web information",
        tags=["tool_selection", "web"],
        severity=Severity.IMPORTANT,
        input="Search the web for the latest Python release",
        script=[
            [{"name": "web_search", "arguments": {"query": "latest Python release"}}],
            "According to search results, the latest Python release is 3.13.",
        ],
        assertions=[
            ScenarioAssertion(type="tool_called", name="web_search"),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
    EvalScenario(
        id="tool_select_multi_tool_chain",
        name="Multi-tool chain",
        description="Agent correctly chains multiple tools in sequence",
        tags=["tool_selection", "chain"],
        severity=Severity.CRITICAL,
        input="Read config.json and then write a summary to summary.txt",
        script=[
            [{"name": "read_file", "arguments": {"path": "config.json"}}],
            [{"name": "write_file", "arguments": {"path": "summary.txt", "content": "Config summary"}}],
            "Done! I read config.json and wrote a summary to summary.txt.",
        ],
        assertions=[
            ScenarioAssertion(type="tool_called", name="read_file"),
            ScenarioAssertion(type="tool_called", name="write_file"),
            ScenarioAssertion(type="tool_order", order=["read_file", "write_file"]),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
    EvalScenario(
        id="tool_select_no_tool_needed",
        name="No tool needed",
        description="Agent responds directly without calling tools for simple questions",
        tags=["tool_selection", "basic"],
        severity=Severity.IMPORTANT,
        input="What is 2 + 2?",
        script=["2 + 2 equals 4."],
        assertions=[
            ScenarioAssertion(type="tool_not_called", name="read_file"),
            ScenarioAssertion(type="tool_not_called", name="python_execute"),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
    EvalScenario(
        id="tool_select_web_fetch_after_search",
        name="Search then fetch",
        description="Agent chains web_search → web_fetch for detailed info",
        tags=["tool_selection", "web", "chain"],
        severity=Severity.IMPORTANT,
        input="Find and read the Python 3.13 release notes",
        script=[
            [{"name": "web_search", "arguments": {"query": "Python 3.13 release notes"}}],
            [{"name": "web_fetch", "arguments": {"url": "https://docs.python.org/3.13/whatsnew/3.13.html"}}],
            "Here's a summary of the Python 3.13 release notes...",
        ],
        assertions=[
            ScenarioAssertion(type="tool_order", order=["web_search", "web_fetch"]),
            ScenarioAssertion(type="finish_reason", reason="stop"),
        ],
    ),
]
