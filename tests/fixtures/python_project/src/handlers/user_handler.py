"""User request handler with input validation."""

from __future__ import annotations

import logging
from typing import Any, Optional

from src.errors.app_error import AppError, ValidationError
from src.models.user import UserRole
from src.services.user_service import UserService

logger = logging.getLogger(__name__)

ALLOWED_ROLES: frozenset[str] = frozenset({"admin", "editor", "viewer"})


class UserHandler:
    """Handles user-related HTTP requests."""

    def __init__(self, user_service: UserService) -> None:
        self._service = user_service

    def get_user(self, user_id: str) -> dict[str, Any]:
        """Handle GET /users/:id."""
        logger.info("GET user request", extra={"user_id": user_id})
        try:
            user = self._service.get_user(user_id)
            return {"data": user.model_dump()}
        except AppError as exc:
            logger.warning("User fetch failed", extra={"error": exc.message})
            return {"error": exc.to_dict()}

    def create_user(self, data: dict[str, Any]) -> dict[str, Any]:
        """Handle POST /users."""
        logger.info("POST user request")

        username = data.get("username")
        email = data.get("email")
        role_str: Optional[str] = data.get("role")

        if not username or not email:
            raise ValidationError("username and email are required")

        role = self._parse_role(role_str)

        try:
            user = self._service.create_user(username=username, email=email, role=role)
            logger.info("User created via handler", extra={"user_id": user.id})
            return {"data": user.model_dump(), "status": "created"}
        except AppError as exc:
            return {"error": exc.to_dict()}

    @staticmethod
    def _parse_role(role_str: Optional[str]) -> UserRole:
        """Parse and validate role string."""
        if role_str is None:
            return UserRole.VIEWER

        normalized = role_str.strip().lower()
        if normalized not in ALLOWED_ROLES:
            raise ValidationError(
                f"Invalid role: {role_str}. Must be one of: {', '.join(sorted(ALLOWED_ROLES))}",
                field="role",
            )

        role_map: dict[str, UserRole] = {
            "admin": UserRole.ADMIN,
            "editor": UserRole.EDITOR,
            "viewer": UserRole.VIEWER,
        }
        return role_map[normalized]
