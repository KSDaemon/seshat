"""Tests for user service business logic."""

from __future__ import annotations

from typing import Optional
from unittest.mock import MagicMock

import pytest

from src.errors.app_error import NotFoundError, ValidationError
from src.models.user import User, UserRole, UserStatus
from src.services.user_service import UserService


def _make_user(
    username: str = "testuser",
    email: str = "test@example.com",
    role: UserRole = UserRole.VIEWER,
) -> User:
    """Create a test user with defaults."""
    return User(username=username, email=email, role=role)


class TestGetUser:
    """Tests for UserService.get_user."""

    def test_returns_existing_user(self) -> None:
        """Should return user when found."""
        user = _make_user()
        repo = MagicMock()
        repo.find_by_id.return_value = user
        service = UserService(repository=repo)

        result = service.get_user(user.id)

        assert result.username == "testuser"
        repo.find_by_id.assert_called_once_with(user.id)

    def test_raises_not_found(self) -> None:
        """Should raise NotFoundError when user doesn't exist."""
        repo = MagicMock()
        repo.find_by_id.return_value = None
        service = UserService(repository=repo)

        with pytest.raises(NotFoundError) as exc_info:
            service.get_user("nonexistent")

        assert "not found" in str(exc_info.value)


class TestCreateUser:
    """Tests for UserService.create_user."""

    def test_creates_new_user(self) -> None:
        """Should create and return new user."""
        repo = MagicMock()
        repo.find_by_email.return_value = None
        repo.save.side_effect = lambda u: u
        service = UserService(repository=repo)

        result = service.create_user("newuser", "new@example.com")

        assert result.username == "newuser"
        assert result.role == UserRole.VIEWER

    def test_rejects_duplicate_email(self) -> None:
        """Should raise ValidationError for duplicate email."""
        existing = _make_user()
        repo = MagicMock()
        repo.find_by_email.return_value = existing
        service = UserService(repository=repo)

        with pytest.raises(ValidationError):
            service.create_user("other", "test@example.com")

    def test_creates_with_custom_role(self) -> None:
        """Should create user with specified role."""
        repo = MagicMock()
        repo.find_by_email.return_value = None
        repo.save.side_effect = lambda u: u
        service = UserService(repository=repo)

        result = service.create_user("admin", "admin@example.com", role=UserRole.ADMIN)

        assert result.role == UserRole.ADMIN


class TestUpdateRole:
    """Tests for UserService.update_role."""

    def test_updates_role(self) -> None:
        """Should update user role."""
        user = _make_user()
        repo = MagicMock()
        repo.find_by_id.return_value = user
        repo.save.side_effect = lambda u: u
        service = UserService(repository=repo)

        result = service.update_role(user.id, UserRole.EDITOR)

        assert result.role == UserRole.EDITOR


class TestDeactivateUser:
    """Tests for UserService.deactivate_user."""

    def test_deactivates_user(self) -> None:
        """Should set user status to inactive."""
        user = _make_user()
        repo = MagicMock()
        repo.find_by_id.return_value = user
        repo.save.side_effect = lambda u: u
        service = UserService(repository=repo)

        result = service.deactivate_user(user.id)

        assert result.status == UserStatus.INACTIVE
