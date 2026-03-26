"""Service layer with business logic."""

import logging
from typing import Optional, List

from .models import User, Role, Config


logger = logging.getLogger(__name__)


class UserService:
    """Manages user operations."""

    def __init__(self, config: Config):
        self.config = config
        self._users: List[User] = []

    def add_user(self, user: User) -> None:
        self._users.append(user)
        logger.info("Added user: %s", user.name)

    def find_user(self, name: str) -> Optional[User]:
        for user in self._users:
            if user.name == name:
                return user
        return None

    async def sync_users(self) -> int:
        """Sync users from external source."""
        pass


class AdminService(UserService):
    """Extended service for admin operations."""

    def promote_user(self, user: User) -> None:
        user.role = Role.ADMIN


def create_default_service() -> UserService:
    config = Config()
    return UserService(config)
