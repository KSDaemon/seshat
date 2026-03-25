"""User service with business logic and logging."""

from __future__ import annotations

import logging
from typing import Optional, Protocol

from src.errors.app_error import NotFoundError, ValidationError
from src.models.user import User, UserRole

logger = logging.getLogger(__name__)


class UserRepository(Protocol):
    """Repository protocol for user persistence."""

    def find_by_id(self, user_id: str) -> Optional[User]: ...
    def find_by_email(self, email: str) -> Optional[User]: ...
    def save(self, user: User) -> User: ...
    def delete(self, user_id: str) -> bool: ...


class UserService:
    """Handles user-related business operations."""

    def __init__(self, repository: UserRepository) -> None:
        self._repository = repository

    def get_user(self, user_id: str) -> User:
        """Retrieve a user by ID."""
        logger.info("Fetching user", extra={"user_id": user_id})
        user = self._repository.find_by_id(user_id)
        if user is None:
            logger.warning("User not found", extra={"user_id": user_id})
            raise NotFoundError("User", user_id)
        return user

    def create_user(
        self, username: str, email: str, role: UserRole = UserRole.VIEWER
    ) -> User:
        """Create a new user with validation."""
        logger.info("Creating user", extra={"username": username, "email": email})

        existing = self._repository.find_by_email(email)
        if existing is not None:
            raise ValidationError(f"Email already in use: {email}", field="email")

        user = User(username=username, email=email, role=role)
        saved = self._repository.save(user)
        logger.info("User created", extra={"user_id": saved.id, "role": role.value})
        return saved

    def update_role(self, user_id: str, new_role: UserRole) -> User:
        """Update a user's role."""
        logger.info(
            "Updating user role",
            extra={"user_id": user_id, "new_role": new_role.value},
        )
        user = self.get_user(user_id)
        updated = user.model_copy(update={"role": new_role})
        return self._repository.save(updated)

    def deactivate_user(self, user_id: str) -> User:
        """Deactivate a user account."""
        logger.warning("Deactivating user", extra={"user_id": user_id})
        user = self.get_user(user_id)
        from src.models.user import UserStatus

        updated = user.model_copy(update={"status": UserStatus.INACTIVE})
        return self._repository.save(updated)
