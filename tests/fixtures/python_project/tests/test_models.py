"""Tests for domain models."""

from __future__ import annotations

import pytest

from src.models.user import User, UserRole, UserStatus
from src.models.project import Project, ProjectStatus


class TestUser:
    """Tests for User model."""

    def test_create_user(self) -> None:
        """Should create user with defaults."""
        user = User(username="testuser", email="test@example.com")
        assert user.username == "testuser"
        assert user.role == UserRole.VIEWER
        assert user.status == UserStatus.ACTIVE
        assert user.login_count == 0

    def test_email_validation(self) -> None:
        """Should reject invalid email."""
        with pytest.raises(ValueError, match="Invalid email"):
            User(username="testuser", email="not-an-email")

    def test_email_normalized(self) -> None:
        """Should lowercase email."""
        user = User(username="testuser", email="Test@Example.COM")
        assert user.email == "test@example.com"

    def test_is_active(self) -> None:
        """Should return True for active users."""
        user = User(username="testuser", email="test@example.com")
        assert user.is_active is True

    def test_has_permission(self) -> None:
        """Should check role hierarchy."""
        admin = User(username="admin", email="admin@example.com", role=UserRole.ADMIN)
        assert admin.has_permission(UserRole.EDITOR) is True
        assert admin.has_permission(UserRole.ADMIN) is True

    def test_viewer_lacks_admin_permission(self) -> None:
        """Viewer should not have admin permission."""
        viewer = User(username="viewer", email="viewer@example.com")
        assert viewer.has_permission(UserRole.ADMIN) is False


class TestProject:
    """Tests for Project model."""

    def test_create_project(self) -> None:
        """Should create project with defaults."""
        project = Project(name="test-project", owner_id="user-1")
        assert project.status == ProjectStatus.DRAFT
        assert project.tags == []

    def test_archive(self) -> None:
        """Should return archive update dict."""
        project = Project(name="test-project", owner_id="user-1")
        update = project.archive()
        assert update["status"] == ProjectStatus.ARCHIVED
        assert update["archived_at"] is not None

    def test_add_tag(self) -> None:
        """Should add normalized tag."""
        project = Project(name="test", owner_id="user-1", tags=["python"])
        new_tags = project.add_tag("  Rust  ")
        assert "rust" in new_tags
        assert len(new_tags) == 2

    def test_add_duplicate_tag(self) -> None:
        """Should not add duplicate tag."""
        project = Project(name="test", owner_id="user-1", tags=["python"])
        new_tags = project.add_tag("python")
        assert len(new_tags) == 1
