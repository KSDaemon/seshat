"""Notification service with logging and async support."""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

logger = logging.getLogger(__name__)


class NotificationChannel(Enum):
    """Supported notification channels."""

    EMAIL = "email"
    WEBHOOK = "webhook"
    IN_APP = "in_app"


@dataclass(frozen=True)
class Notification:
    """Immutable notification payload."""

    recipient_id: str
    channel: NotificationChannel
    subject: str
    body: str
    metadata: dict[str, Any] = field(default_factory=dict)


class NotificationService:
    """Manages sending notifications through various channels."""

    def __init__(self) -> None:
        self._sent: list[Notification] = []

    def send(self, notification: Notification) -> bool:
        """Send a notification and log the result."""
        logger.info(
            "Sending notification",
            extra={
                "recipient": notification.recipient_id,
                "channel": notification.channel.value,
                "subject": notification.subject,
            },
        )

        try:
            self._dispatch(notification)
            self._sent.append(notification)
            logger.info(
                "Notification sent successfully",
                extra={"recipient": notification.recipient_id},
            )
            return True
        except Exception:
            logger.exception(
                "Failed to send notification",
                extra={"recipient": notification.recipient_id},
            )
            return False

    def _dispatch(self, notification: Notification) -> None:
        """Route notification to the appropriate channel handler."""
        handlers: dict[NotificationChannel, str] = {
            NotificationChannel.EMAIL: "_send_email",
            NotificationChannel.WEBHOOK: "_send_webhook",
            NotificationChannel.IN_APP: "_send_in_app",
        }
        handler_name = handlers.get(notification.channel)
        if handler_name is None:
            msg = f"Unsupported channel: {notification.channel}"
            raise ValueError(msg)
        handler = getattr(self, handler_name)
        handler(notification)

    def _send_email(self, notification: Notification) -> None:
        """Send email notification (stub)."""
        logger.debug("Email dispatch", extra={"to": notification.recipient_id})

    def _send_webhook(self, notification: Notification) -> None:
        """Send webhook notification (stub)."""
        logger.debug("Webhook dispatch", extra={"to": notification.recipient_id})

    def _send_in_app(self, notification: Notification) -> None:
        """Send in-app notification (stub)."""
        logger.debug("In-app dispatch", extra={"to": notification.recipient_id})

    @property
    def sent_count(self) -> int:
        """Return count of successfully sent notifications."""
        return len(self._sent)
