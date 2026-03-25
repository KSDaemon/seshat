"""Tests for utility helper functions."""

from __future__ import annotations

import pytest

from src.utils.helpers import chunk_list, slugify, truncate


class TestSlugify:
    """Tests for the slugify function."""

    def test_basic_slug(self) -> None:
        """Should convert simple text to slug."""
        assert slugify("Hello World") == "hello-world"

    def test_special_characters(self) -> None:
        """Should remove special characters."""
        assert slugify("Hello, World! @#$%") == "hello-world"

    def test_unicode_normalization(self) -> None:
        """Should normalize unicode characters."""
        assert slugify("Héllo Wörld") == "hello-world"

    def test_custom_separator(self) -> None:
        """Should use custom separator."""
        assert slugify("Hello World", separator="_") == "hello_world"

    def test_empty_string(self) -> None:
        """Should handle empty string."""
        assert slugify("") == ""

    def test_max_length(self) -> None:
        """Should truncate to MAX_SLUG_LENGTH."""
        long_text = "a" * 200
        result = slugify(long_text)
        assert len(result) <= 100


class TestTruncate:
    """Tests for the truncate function."""

    def test_short_text_unchanged(self) -> None:
        """Should not truncate short text."""
        assert truncate("hello", max_length=10) == "hello"

    def test_long_text_truncated(self) -> None:
        """Should truncate long text with suffix."""
        result = truncate("hello world", max_length=8)
        assert result == "hello..."
        assert len(result) == 8

    def test_custom_suffix(self) -> None:
        """Should use custom suffix."""
        result = truncate("hello world", max_length=9, suffix="~")
        assert result == "hello wo~"

    def test_exact_length(self) -> None:
        """Should not truncate at exact max_length."""
        assert truncate("hello", max_length=5) == "hello"


class TestChunkList:
    """Tests for the chunk_list function."""

    def test_even_split(self) -> None:
        """Should split evenly."""
        assert chunk_list([1, 2, 3, 4], 2) == [[1, 2], [3, 4]]

    def test_uneven_split(self) -> None:
        """Should handle remainder."""
        assert chunk_list([1, 2, 3, 4, 5], 2) == [[1, 2], [3, 4], [5]]

    def test_empty_list(self) -> None:
        """Should return empty for empty input."""
        assert chunk_list([], 3) == []

    def test_invalid_chunk_size(self) -> None:
        """Should raise ValueError for non-positive chunk_size."""
        with pytest.raises(ValueError, match="chunk_size must be positive"):
            chunk_list([1, 2], 0)
