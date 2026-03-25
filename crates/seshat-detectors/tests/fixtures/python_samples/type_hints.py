# Sample: Python type hint patterns and conventions
# Expected detections: type_hints, generics, TypeVar, Protocol, dataclass_types, Optional, Union_alternatives

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Generic, Optional, Protocol, TypeVar

T = TypeVar("T")
K = TypeVar("K")
V = TypeVar("V")


class Repository(Protocol[T]):
    """Generic repository protocol."""

    def find_by_id(self, entity_id: str) -> Optional[T]: ...
    def save(self, entity: T) -> T: ...
    def delete(self, entity_id: str) -> bool: ...
    def list_all(self) -> list[T]: ...


@dataclass(frozen=True)
class Result(Generic[T]):
    """Result type for operations that may fail."""

    value: T | None = None
    error: str | None = None
    metadata: dict[str, object] = field(default_factory=dict)

    @property
    def is_ok(self) -> bool:
        """Check if result is successful."""
        return self.error is None

    @property
    def is_err(self) -> bool:
        """Check if result is an error."""
        return self.error is not None

    @classmethod
    def ok(cls, value: T) -> Result[T]:
        """Create a success result."""
        return cls(value=value)

    @classmethod
    def err(cls, error: str) -> Result[T]:
        """Create an error result."""
        return cls(error=error)


@dataclass
class Pair(Generic[K, V]):
    """Generic key-value pair."""

    key: K
    value: V

    def swap(self) -> Pair[V, K]:
        """Return a new pair with key and value swapped."""
        return Pair(key=self.value, value=self.key)


def first_or_none(items: list[T]) -> T | None:
    """Return first item or None."""
    return items[0] if items else None


def merge_dicts(base: dict[str, V], override: dict[str, V]) -> dict[str, V]:
    """Merge two dicts, override takes precedence."""
    return {**base, **override}


class Validator(Protocol):
    """Validator protocol for input checking."""

    def validate(self, value: object) -> bool: ...
    def error_message(self) -> str: ...
