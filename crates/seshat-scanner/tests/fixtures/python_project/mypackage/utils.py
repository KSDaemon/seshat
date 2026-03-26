"""Utility functions."""

import os
import re
from pathlib import Path
from collections.abc import Mapping


def format_name(first: str, last: str) -> str:
    """Format a full name."""
    return f"{first} {last}"


def load_env(path: Path) -> dict:
    """Load environment variables from a file."""
    result = {}
    if path.exists():
        with open(path) as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#"):
                    key, _, value = line.partition("=")
                    result[key.strip()] = value.strip()
    return result


async def fetch_remote_config(url: str) -> dict:
    """Fetch configuration from a remote URL."""
    pass


def _private_helper(data):
    """Internal helper — no type hints."""
    return str(data)
