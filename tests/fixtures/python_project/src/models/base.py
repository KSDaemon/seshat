"""Base model with common fields and mixins."""

from __future__ import annotations

import uuid
from datetime import datetime, timezone
from typing import Any, ClassVar

from pydantic import BaseModel as PydanticBaseModel
from pydantic import ConfigDict, Field


class BaseModel(PydanticBaseModel):
    """Base model with common configuration."""

    model_config: ClassVar[ConfigDict] = ConfigDict(
        frozen=True,
        str_strip_whitespace=True,
        validate_assignment=True,
    )

    id: str = Field(default_factory=lambda: str(uuid.uuid4()))


class TimestampMixin(PydanticBaseModel):
    """Mixin that adds created_at and updated_at fields."""

    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    updated_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))

    def touch(self) -> dict[str, Any]:
        """Return dict with updated timestamp."""
        return {"updated_at": datetime.now(timezone.utc)}


MAX_DESCRIPTION_LENGTH: int = 500
DEFAULT_PAGE_SIZE: int = 20
SUPPORTED_SORT_FIELDS: tuple[str, ...] = ("created_at", "updated_at", "name")
