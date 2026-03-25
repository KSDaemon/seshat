# Sample: Python import grouping conventions (stdlib, third-party, local)
# Expected detections: grouped_imports, stdlib_first, third_party_second, local_third, __future___first

from __future__ import annotations

import logging
import os
import re
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

import httpx
from pydantic import BaseModel, Field
from sqlalchemy import Column, Integer, String

from src.models.base import BaseModel as AppBaseModel
from src.utils.helpers import slugify


logger = logging.getLogger(__name__)


class ImportExample(BaseModel):
    """Example class demonstrating import usage."""

    name: str
    path: Optional[Path] = None
    created_at: datetime = Field(default_factory=lambda: datetime.now(timezone.utc))
    tags: dict[str, Any] = Field(default_factory=dict)

    def process(self) -> str:
        """Use imported modules."""
        slug = slugify(self.name)
        pattern = re.compile(r"[a-z]+")
        env_value = os.environ.get("APP_ENV", "development")
        matches = pattern.findall(slug)
        logger.info(
            "Processed %s: %d matches in %s", self.name, len(matches), env_value
        )
        return slug


def fetch_data(url: str) -> dict[str, Any]:
    """Example function using httpx."""
    counts: dict[str, int] = defaultdict(int)
    response = httpx.get(url)
    counts["requests"] += 1
    return {"status": response.status_code, "counts": dict(counts)}
