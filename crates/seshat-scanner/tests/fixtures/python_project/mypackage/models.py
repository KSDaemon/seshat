"""Data models for the application."""

from dataclasses import dataclass, field
from typing import Optional, List
from enum import Enum


class Role(Enum):
    ADMIN = "admin"
    USER = "user"
    GUEST = "guest"


@dataclass
class User:
    name: str
    email: str
    role: Role = Role.USER
    age: Optional[int] = None

    def display_name(self) -> str:
        return f"{self.name} ({self.email})"

    async def fetch_profile(self) -> dict:
        pass


@dataclass
class Config:
    host: str = "localhost"
    port: int = 8080
    debug: bool = False
    allowed_origins: List[str] = field(default_factory=list)
