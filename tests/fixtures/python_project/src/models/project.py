"""Project domain model."""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Optional

from pydantic import Field

from src.models.base import BaseModel, TimestampMixin
from src.models.user import User


class ProjectStatus:
    """Project status constants."""

    DRAFT = "draft"
    ACTIVE = "active"
    ARCHIVED = "archived"
    COMPLETED = "completed"


ALLOWED_STATUSES: frozenset[str] = frozenset(
    {
        ProjectStatus.DRAFT,
        ProjectStatus.ACTIVE,
        ProjectStatus.ARCHIVED,
        ProjectStatus.COMPLETED,
    }
)


class Project(BaseModel, TimestampMixin):
    """Project entity representing a tracked codebase."""

    name: str = Field(min_length=1, max_length=100)
    description: Optional[str] = None
    owner_id: str
    status: str = ProjectStatus.DRAFT
    tags: list[str] = Field(default_factory=list)
    metadata: dict[str, str] = Field(default_factory=dict)
    archived_at: Optional[datetime] = None

    def archive(self) -> dict[str, object]:
        """Mark project as archived."""
        return {
            "status": ProjectStatus.ARCHIVED,
            "archived_at": datetime.now(timezone.utc),
        }

    def is_owned_by(self, user: User) -> bool:
        """Check if project is owned by given user."""
        return self.owner_id == user.id

    def add_tag(self, tag: str) -> list[str]:
        """Add a tag if not already present."""
        normalized = tag.strip().lower()
        if normalized and normalized not in self.tags:
            return [*self.tags, normalized]
        return list(self.tags)
