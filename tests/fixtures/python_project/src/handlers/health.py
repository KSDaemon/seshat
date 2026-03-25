"""Health check handlers."""

from __future__ import annotations

import logging
from datetime import datetime, timezone
from typing import Any

logger = logging.getLogger(__name__)

VERSION: str = "0.1.0"
SERVICE_NAME: str = "fixture-app"


def health_check() -> dict[str, Any]:
    """Return basic health status."""
    logger.debug("Health check requested")
    return {
        "status": "healthy",
        "service": SERVICE_NAME,
        "version": VERSION,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }


def readiness_check(*, db_connected: bool = True) -> dict[str, Any]:
    """Return readiness status with dependency checks."""
    checks: dict[str, bool] = {
        "database": db_connected,
    }
    all_ready = all(checks.values())

    if not all_ready:
        logger.warning("Readiness check failed", extra={"checks": checks})

    return {
        "ready": all_ready,
        "checks": checks,
    }
