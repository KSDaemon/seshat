# Sample: Python stdlib logging patterns and conventions
# Expected detections: stdlib_logging, getLogger(__name__), structured_extra, log_levels, exception_logging

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)


def configure_logging(level: str = "INFO") -> None:
    """Configure root logger with standard format."""
    logging.basicConfig(
        level=getattr(logging, level.upper(), logging.INFO),
        format="%(asctime)s - %(name)s - %(levelname)s - %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )


class OrderProcessor:
    """Demonstrates various logging patterns."""

    def __init__(self, processor_id: str) -> None:
        self._id = processor_id
        self._logger = logging.getLogger(f"{__name__}.{self.__class__.__name__}")

    def process_order(self, order_id: str, amount: float) -> bool:
        """Process an order with structured logging."""
        self._logger.info(
            "Processing order",
            extra={"order_id": order_id, "amount": amount, "processor": self._id},
        )

        if amount <= 0:
            self._logger.warning(
                "Invalid order amount",
                extra={"order_id": order_id, "amount": amount},
            )
            return False

        if amount > 10000:
            self._logger.warning(
                "High value order detected",
                extra={"order_id": order_id, "amount": amount},
            )

        try:
            self._execute(order_id, amount)
            self._logger.info(
                "Order processed successfully",
                extra={"order_id": order_id},
            )
            return True
        except Exception:
            self._logger.exception(
                "Order processing failed",
                extra={"order_id": order_id},
            )
            return False

    def _execute(self, order_id: str, amount: float) -> None:
        """Execute order processing (stub)."""
        self._logger.debug(
            "Executing order",
            extra={"order_id": order_id, "step": "execute"},
        )


def audit_log(action: str, user_id: str, details: dict[str, Any]) -> None:
    """Write an audit log entry."""
    audit_logger = logging.getLogger("audit")
    audit_logger.info(
        "Audit event: %s",
        action,
        extra={"user_id": user_id, "action": action, **details},
    )
