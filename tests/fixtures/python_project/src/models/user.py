"""User domain model with validation."""

from __future__ import annotations

import re
from enum import Enum
from typing import Optional

from pydantic import Field, field_validator

from src.models.base import BaseModel, TimestampMixin


class UserRole(Enum):
    """Available user roles."""

    ADMIN = "admin"
    EDITOR = "editor"
    VIEWER = "viewer"


class UserStatus(Enum):
    """User account status."""

    ACTIVE = "active"
    INACTIVE = "inactive"
    SUSPENDED = "suspended"


EMAIL_PATTERN: str = r"^[a-zA-Z0-9_.+-]+@[a-zA-Z0-9-]+\.[a-zA-Z0-9-.]+$"
MAX_USERNAME_LENGTH: int = 50
MIN_USERNAME_LENGTH: int = 3


class User(BaseModel, TimestampMixin):
    """User entity with validation rules."""

    username: str = Field(
        min_length=MIN_USERNAME_LENGTH,
        max_length=MAX_USERNAME_LENGTH,
    )
    email: str
    display_name: Optional[str] = None
    role: UserRole = UserRole.VIEWER
    status: UserStatus = UserStatus.ACTIVE
    login_count: int = Field(default=0, ge=0)

    @field_validator("email")
    @classmethod
    def validate_email(cls, value: str) -> str:
        """Validate email format."""
        if not re.match(EMAIL_PATTERN, value):
            msg = f"Invalid email format: {value}"
            raise ValueError(msg)
        return value.lower()

    @property
    def is_active(self) -> bool:
        """Check if user account is active."""
        return self.status == UserStatus.ACTIVE

    def has_permission(self, required_role: UserRole) -> bool:
        """Check if user has at least the required role level."""
        role_hierarchy: dict[UserRole, int] = {
            UserRole.VIEWER: 0,
            UserRole.EDITOR: 1,
            UserRole.ADMIN: 2,
        }
        return role_hierarchy[self.role] >= role_hierarchy[required_role]
