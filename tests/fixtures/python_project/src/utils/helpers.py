"""General utility functions."""

import re
import unicodedata
from typing import TypeVar

T = TypeVar("T")

SLUG_SEPARATOR: str = "-"
MAX_SLUG_LENGTH: int = 100


def slugify(text: str, *, separator: str = SLUG_SEPARATOR) -> str:
    """Convert text to URL-safe slug."""
    normalized = unicodedata.normalize("NFKD", text)
    ascii_text = normalized.encode("ascii", "ignore").decode("ascii")
    lowered = ascii_text.lower()
    cleaned = re.sub(r"[^a-z0-9]+", separator, lowered)
    stripped = cleaned.strip(separator)
    return stripped[:MAX_SLUG_LENGTH]


def truncate(text: str, max_length: int = 100, *, suffix: str = "...") -> str:
    """Truncate text to max_length, appending suffix if truncated."""
    if len(text) <= max_length:
        return text
    truncated_length = max_length - len(suffix)
    if truncated_length <= 0:
        return suffix[:max_length]
    return text[:truncated_length] + suffix


def chunk_list(items: list[T], chunk_size: int) -> list[list[T]]:
    """Split a list into chunks of specified size."""
    if chunk_size <= 0:
        msg = "chunk_size must be positive"
        raise ValueError(msg)
    return [items[i : i + chunk_size] for i in range(0, len(items), chunk_size)]
