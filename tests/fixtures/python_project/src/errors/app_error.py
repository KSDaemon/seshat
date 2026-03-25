"""Custom error hierarchy with typed error codes."""

from __future__ import annotations

from enum import Enum
from typing import Any, Optional


class ErrorCode(Enum):
    """Application-wide error codes."""

    VALIDATION_FAILED = "VALIDATION_FAILED"
    NOT_FOUND = "NOT_FOUND"
    UNAUTHORIZED = "UNAUTHORIZED"
    FORBIDDEN = "FORBIDDEN"
    CONFLICT = "CONFLICT"
    INTERNAL = "INTERNAL_ERROR"


class AppError(Exception):
    """Base application error with structured context."""

    def __init__(
        self,
        message: str,
        code: ErrorCode = ErrorCode.INTERNAL,
        details: Optional[dict[str, Any]] = None,
    ) -> None:
        super().__init__(message)
        self.message = message
        self.code = code
        self.details = details or {}

    def to_dict(self) -> dict[str, Any]:
        """Serialize error to dictionary for API responses."""
        return {
            "error": self.code.value,
            "message": self.message,
            "details": self.details,
        }


class ValidationError(AppError):
    """Raised when input validation fails."""

    def __init__(self, message: str, field: Optional[str] = None) -> None:
        details: dict[str, Any] = {}
        if field:
            details["field"] = field
        super().__init__(message, code=ErrorCode.VALIDATION_FAILED, details=details)


class NotFoundError(AppError):
    """Raised when a requested resource is not found."""

    def __init__(self, resource: str, identifier: str) -> None:
        message = f"{resource} with id '{identifier}' not found"
        super().__init__(
            message,
            code=ErrorCode.NOT_FOUND,
            details={"resource": resource, "id": identifier},
        )


class AuthorizationError(AppError):
    """Raised when user lacks required permissions."""

    def __init__(self, action: str, resource: str) -> None:
        message = f"Not authorized to {action} on {resource}"
        super().__init__(
            message,
            code=ErrorCode.FORBIDDEN,
            details={"action": action, "resource": resource},
        )
