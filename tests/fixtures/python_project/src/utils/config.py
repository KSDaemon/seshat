"""Application configuration with environment variable support."""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from typing import Optional

DEFAULT_HOST: str = "127.0.0.1"
DEFAULT_PORT: int = 8080
DEFAULT_LOG_LEVEL: str = "INFO"
DEFAULT_DB_URL: str = "sqlite:///data.db"


@dataclass(frozen=True)
class DatabaseConfig:
    """Database connection settings."""

    url: str = DEFAULT_DB_URL
    pool_size: int = 5
    echo: bool = False


@dataclass(frozen=True)
class ServerConfig:
    """HTTP server settings."""

    host: str = DEFAULT_HOST
    port: int = DEFAULT_PORT
    debug: bool = False


@dataclass(frozen=True)
class Settings:
    """Application settings loaded from environment."""

    database: DatabaseConfig = field(default_factory=DatabaseConfig)
    server: ServerConfig = field(default_factory=ServerConfig)
    log_level: str = DEFAULT_LOG_LEVEL
    secret_key: Optional[str] = None

    @classmethod
    def from_env(cls) -> Settings:
        """Load settings from environment variables."""
        db_url = os.environ.get("DATABASE_URL", DEFAULT_DB_URL)
        host = os.environ.get("HOST", DEFAULT_HOST)
        port = int(os.environ.get("PORT", str(DEFAULT_PORT)))
        log_level = os.environ.get("LOG_LEVEL", DEFAULT_LOG_LEVEL)
        secret_key = os.environ.get("SECRET_KEY")
        debug = os.environ.get("DEBUG", "").lower() in ("1", "true", "yes")

        return cls(
            database=DatabaseConfig(url=db_url),
            server=ServerConfig(host=host, port=port, debug=debug),
            log_level=log_level,
            secret_key=secret_key,
        )


_settings: Optional[Settings] = None


def get_settings() -> Settings:
    """Get or create singleton settings instance."""
    global _settings  # noqa: PLW0603
    if _settings is None:
        _settings = Settings.from_env()
    return _settings
