#!/usr/bin/env python3
"""
MCP smoke testing and debugging tool for Seshat.

Usage:
  python tools/mcp-smoke.py list -- seshat serve
  python tools/mcp-smoke.py call query_code_pattern query=handleRequest kind=function -- seshat serve
  python tools/mcp-smoke.py call validate_approach description="add new handler" -- seshat serve
  python tools/mcp-smoke.py smoke -- seshat serve
  python tools/mcp-smoke.py baseline -- seshat serve
  python tools/mcp-smoke.py diff -- seshat serve

Key=value params are automatically coerced: integers stay integers, everything
else is a string. For the rare case of nested JSON (e.g. the `examples` field
in record_decision / update_decision), use --params with a raw JSON string:

  python tools/mcp-smoke.py call record_decision \\
    description="Use anyhow for errors" \\
    --params '{"examples":[{"file":"src/main.rs","line":10}]}' \\
    -- seshat serve

  (key=value args and --params are merged; --params wins on conflicts)
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
import time
from pathlib import Path
from typing import Any

# ---------------------------------------------------------------------------
# Dependency check
# ---------------------------------------------------------------------------

try:
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client
except ImportError:
    print(
        "Error: 'mcp' package not found.\n"
        "Install it with:  pip install 'mcp[cli]>=1.0.0'\n"
        "Or:               pip install -r tools/requirements.txt",
        file=sys.stderr,
    )
    sys.exit(1)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

SCENARIOS_FILE = Path(__file__).parent / "mcp-scenarios.json"
BASELINE_FILE = Path(__file__).parent / "mcp-baseline.json"

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------


def split_on_separator(argv: list[str]) -> tuple[list[str], list[str]]:
    """Split argv on '--' into (our_args, server_cmd)."""
    try:
        idx = argv.index("--")
        return argv[:idx], argv[idx + 1 :]
    except ValueError:
        return argv, []


def parse_kv(args: list[str]) -> dict[str, Any]:
    """Parse ['key=value', ...] into a dict, auto-coercing integers."""
    params: dict[str, Any] = {}
    for arg in args:
        if "=" not in arg:
            print(f"Warning: ignoring argument without '=': {arg!r}", file=sys.stderr)
            continue
        key, _, raw = arg.partition("=")
        key = key.strip()
        # Try integer coercion
        try:
            params[key] = int(raw)
        except ValueError:
            params[key] = raw
    return params


def usage() -> None:
    print(__doc__)
    sys.exit(0)


# ---------------------------------------------------------------------------
# MCP connection
# ---------------------------------------------------------------------------


async def connect_and_run(server_cmd: list[str], coro_factory):
    """
    Spawn the MCP server via stdio and run coro_factory(session).
    server_cmd is e.g. ['seshat', 'serve'] or ['./target/debug/seshat', 'serve'].
    """
    if not server_cmd:
        print("Error: no server command specified after '--'.", file=sys.stderr)
        print(
            "Example:  python tools/mcp-smoke.py list -- seshat serve", file=sys.stderr
        )
        sys.exit(1)

    params = StdioServerParameters(command=server_cmd[0], args=server_cmd[1:])
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            return await coro_factory(session)


# ---------------------------------------------------------------------------
# Formatting helpers
# ---------------------------------------------------------------------------

RESET = "\033[0m"
BOLD = "\033[1m"
GREEN = "\033[32m"
RED = "\033[31m"
YELLOW = "\033[33m"
CYAN = "\033[36m"
DIM = "\033[2m"


def _color(text: str, code: str) -> str:
    if sys.stdout.isatty():
        return f"{code}{text}{RESET}"
    return text


def ok(text: str) -> str:
    return _color(text, GREEN)


def err(text: str) -> str:
    return _color(text, RED)


def warn(text: str) -> str:
    return _color(text, YELLOW)


def bold(text: str) -> str:
    return _color(text, BOLD)


def dim(text: str) -> str:
    return _color(text, DIM)


def cyan(text: str) -> str:
    return _color(text, CYAN)


def _extract_text(result) -> str:
    """Extract plain text from an MCP CallToolResult."""
    parts = []
    for item in result.content:
        if hasattr(item, "text"):
            parts.append(item.text)
    return "\n".join(parts)


def _parse_result_json(result) -> Any:
    """Extract and parse JSON from a CallToolResult, or return raw text."""
    text = _extract_text(result)
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text


# ---------------------------------------------------------------------------
# Command: list
# ---------------------------------------------------------------------------


async def cmd_list(session: ClientSession) -> None:
    response = await session.list_tools()
    tools = response.tools

    print(bold(f"\n{len(tools)} tools registered:\n"))
    for tool in tools:
        print(f"  {cyan(tool.name)}")
        if tool.description:
            # First line of description only
            first_line = tool.description.strip().splitlines()[0]
            print(f"    {dim(first_line)}")

        schema = tool.inputSchema or {}
        props = schema.get("properties", {})
        required = set(schema.get("required", []))

        if props:
            for param, info in props.items():
                req_marker = bold("*") if param in required else " "
                ptype = info.get("type", "any")
                desc = info.get("description", "")
                # Trim description to one line
                desc_short = desc.splitlines()[0] if desc else ""
                print(
                    f"    {req_marker} {param}  {dim(f'({ptype})')}  {dim(desc_short)}"
                )
        print()


# ---------------------------------------------------------------------------
# Command: call
# ---------------------------------------------------------------------------


async def cmd_call(
    session: ClientSession,
    tool_name: str,
    params: dict[str, Any],
) -> Any:
    t0 = time.monotonic()
    result = await session.call_tool(tool_name, params)
    elapsed = (time.monotonic() - t0) * 1000

    data = _parse_result_json(result)
    print(json.dumps(data, indent=2, ensure_ascii=False))
    print(dim(f"\n({elapsed:.0f}ms)"), file=sys.stderr)
    return data


# ---------------------------------------------------------------------------
# Command: smoke
# ---------------------------------------------------------------------------


def _assert_scenario(data: Any, assertions: dict) -> list[str]:
    """Run assertions against the result. Returns list of failure messages."""
    failures = []

    if not isinstance(data, dict):
        # Try to detect error envelopes
        if isinstance(data, str) and "error" in data.lower():
            failures.append(f"Response is an error string: {data[:120]}")
        return failures

    status = data.get("status", "")
    # If `allow_error` is set, skip the early-exit on error responses.
    # Useful for testing that the server responds correctly even for
    # inputs that are expected to produce an error (e.g. unknown file path).
    if status == "error" and not assertions.get("allow_error", False):
        failures.append(f"Tool returned error: {data.get('error', data)}")
        return failures

    for key in assertions.get("has_keys", []):
        # Support nested dot-notation: "data.results"
        parts = key.split(".")
        node = data
        found = True
        for part in parts:
            if isinstance(node, dict) and part in node:
                node = node[part]
            else:
                found = False
                break
        if not found:
            failures.append(f"Missing expected key: '{key}'")

    for key, expected in assertions.get("equals", {}).items():
        actual = data.get(key)
        if actual != expected:
            failures.append(f"'{key}': expected {expected!r}, got {actual!r}")

    for key in assertions.get("not_empty", []):
        val = data.get(key)
        if not val:
            failures.append(f"Expected '{key}' to be non-empty, got: {val!r}")

    return failures


async def cmd_smoke(
    session: ClientSession,
    scenarios: list[dict],
    *,
    capture: bool = False,
) -> tuple[bool, dict[str, Any]]:
    """
    Run all scenarios. If capture=True, return outputs keyed by scenario name.
    Returns (all_passed, outputs).
    """
    outputs: dict[str, Any] = {}
    passed = 0
    failed = 0
    width = max((len(s["name"]) for s in scenarios), default=40)

    print(bold(f"\nRunning {len(scenarios)} smoke scenarios:\n"))
    ruler = "─" * (width + 30)
    print(dim(ruler))

    for scenario in scenarios:
        name = scenario["name"]
        tool = scenario["tool"]
        params = scenario.get("params", {})
        assertions = scenario.get("assert", {})

        t0 = time.monotonic()
        try:
            result = await session.call_tool(tool, params)
            data = _parse_result_json(result)
            elapsed = (time.monotonic() - t0) * 1000

            failures = _assert_scenario(data, assertions)
            outputs[name] = data

            if failures:
                print(f"  {err('✗')}  {name:<{width}}  {dim(f'({elapsed:.0f}ms)')}")
                for f in failures:
                    print(f"     {err('→')} {f}")
                failed += 1
            else:
                print(f"  {ok('✓')}  {name:<{width}}  {dim(f'({elapsed:.0f}ms)')}")
                passed += 1

        except Exception as exc:
            elapsed = (time.monotonic() - t0) * 1000
            print(f"  {err('✗')}  {name:<{width}}  {dim(f'({elapsed:.0f}ms)')}")
            print(f"     {err('→')} Exception: {exc}")
            outputs[name] = {"__exception__": str(exc)}
            failed += 1

    print(dim(ruler))
    total = passed + failed
    summary = f"{passed}/{total} passed"
    if failed == 0:
        print(f"\n{ok('PASS')}  {summary}\n")
    else:
        print(f"\n{err('FAIL')}  {summary}  ({failed} failed)\n")

    return failed == 0, outputs


# ---------------------------------------------------------------------------
# Command: baseline
# ---------------------------------------------------------------------------


async def cmd_baseline(session: ClientSession, scenarios: list[dict]) -> None:
    print(f"Capturing baseline from {len(scenarios)} scenarios...")
    _, outputs = await cmd_smoke(session, scenarios, capture=True)

    baseline = {
        "captured_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "scenarios": outputs,
    }
    BASELINE_FILE.write_text(json.dumps(baseline, indent=2, ensure_ascii=False) + "\n")
    print(ok(f"\nBaseline saved to {BASELINE_FILE}"))


# ---------------------------------------------------------------------------
# Command: diff
# ---------------------------------------------------------------------------


def _diff_values(name: str, old: Any, new: Any, path: str = "") -> list[str]:
    """Recursively compare two JSON values; return human-readable diff lines."""
    diffs = []
    full = f"{name}.{path}" if path else name

    if type(old) != type(new):
        diffs.append(
            f"  {full}: type changed {type(old).__name__} → {type(new).__name__}"
        )
        return diffs

    if isinstance(old, dict):
        all_keys = set(old) | set(new)
        for k in sorted(all_keys):
            child = f"{path}.{k}" if path else k
            if k not in old:
                diffs.append(f"  {full}: new key '{k}' appeared")
            elif k not in new:
                diffs.append(f"  {full}: key '{k}' disappeared")
            else:
                diffs.extend(_diff_values(name, old[k], new[k], child))
    elif isinstance(old, list):
        if len(old) != len(new):
            diffs.append(f"  {full}: list length {len(old)} → {len(new)}")
        # Don't recurse into list items — too noisy for regression checks
    else:
        if old != new:
            old_s = repr(old)[:80]
            new_s = repr(new)[:80]
            diffs.append(f"  {full}: {old_s} → {new_s}")

    return diffs


async def cmd_diff(session: ClientSession, scenarios: list[dict]) -> None:
    if not BASELINE_FILE.exists():
        print(
            err(f"No baseline found at {BASELINE_FILE}.\n")
            + "Run  python tools/mcp-smoke.py baseline -- seshat serve  first.",
            file=sys.stderr,
        )
        sys.exit(1)

    baseline = json.loads(BASELINE_FILE.read_text())
    baseline_scenarios = baseline.get("scenarios", {})
    captured_at = baseline.get("captured_at", "unknown")
    print(dim(f"Comparing against baseline captured at {captured_at}\n"))

    _, current = await cmd_smoke(session, scenarios, capture=True)

    regressions = []
    for name, new_data in current.items():
        if name not in baseline_scenarios:
            print(warn(f"  ~ {name}: not in baseline (new scenario)"))
            continue
        old_data = baseline_scenarios[name]
        diffs = _diff_values(name, old_data, new_data)
        if diffs:
            regressions.append((name, diffs))

    if not regressions:
        print(ok("No regressions detected. Output matches baseline.\n"))
        return

    print(err(f"\n{len(regressions)} regression(s) detected:\n"))
    ruler = "─" * 60
    for name, diffs in regressions:
        print(bold(f"  {name}"))
        print(dim(f"  {ruler}"))
        for d in diffs:
            print(err(d))
        print()
    sys.exit(1)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def main() -> None:
    raw_args, server_cmd = split_on_separator(sys.argv[1:])

    if not raw_args or raw_args[0] in ("-h", "--help"):
        usage()

    command = raw_args[0]
    rest = raw_args[1:]

    if command == "list":
        asyncio.run(connect_and_run(server_cmd, cmd_list))

    elif command == "call":
        if not rest:
            print(
                "Error: 'call' requires a tool name.\n"
                "Usage: mcp-smoke.py call <tool> [key=value ...] [--params '<json>'] -- <server...>",
                file=sys.stderr,
            )
            sys.exit(1)

        tool_name = rest[0]
        remaining = rest[1:]

        # Split out --params '<json>' if present
        raw_json_override: dict[str, Any] = {}
        kv_args = []
        i = 0
        while i < len(remaining):
            if remaining[i] == "--params" and i + 1 < len(remaining):
                try:
                    raw_json_override = json.loads(remaining[i + 1])
                except json.JSONDecodeError as e:
                    print(f"Error: invalid JSON in --params: {e}", file=sys.stderr)
                    sys.exit(1)
                i += 2
            else:
                kv_args.append(remaining[i])
                i += 1

        params = parse_kv(kv_args)
        params.update(raw_json_override)  # --params wins on conflicts

        asyncio.run(
            connect_and_run(server_cmd, lambda s: cmd_call(s, tool_name, params))
        )

    elif command in ("smoke", "baseline", "diff"):
        if not SCENARIOS_FILE.exists():
            print(
                f"Error: scenarios file not found: {SCENARIOS_FILE}\n"
                "Create tools/mcp-scenarios.json with your test scenarios.",
                file=sys.stderr,
            )
            sys.exit(1)

        scenarios = json.loads(SCENARIOS_FILE.read_text())

        if command == "smoke":

            async def run_smoke(session):
                ok_flag, _ = await cmd_smoke(session, scenarios)
                if not ok_flag:
                    sys.exit(1)

            asyncio.run(connect_and_run(server_cmd, run_smoke))

        elif command == "baseline":
            asyncio.run(
                connect_and_run(server_cmd, lambda s: cmd_baseline(s, scenarios))
            )

        elif command == "diff":
            asyncio.run(connect_and_run(server_cmd, lambda s: cmd_diff(s, scenarios)))

    else:
        print(
            f"Error: unknown command '{command}'. Run with --help for usage.",
            file=sys.stderr,
        )
        sys.exit(1)


if __name__ == "__main__":
    main()
