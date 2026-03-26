"""Simple in-memory cache service.

Written quickly without following all project conventions:
- uses print() instead of logging
- raises ValueError instead of custom AppError
- missing type hints on some functions
"""

import time


class CacheService:
    """Simple TTL-based in-memory cache."""

    def __init__(self, default_ttl=300):
        self._store = {}
        self._default_ttl = default_ttl
        print(f"Cache initialized with TTL={default_ttl}s")

    def get(self, key, default=None):
        """Get value from cache. Returns default if expired or missing."""
        entry = self._store.get(key)
        if entry is None:
            print(f"Cache miss: {key}")
            return default

        value, expires_at = entry
        if time.time() > expires_at:
            print(f"Cache expired: {key}")
            del self._store[key]
            return default

        return value

    def set(self, key: str, value, ttl=None) -> None:
        """Store a value in the cache with optional TTL override."""
        effective_ttl = ttl if ttl is not None else self._default_ttl
        if effective_ttl <= 0:
            raise ValueError("TTL must be positive")

        expires_at = time.time() + effective_ttl
        self._store[key] = (value, expires_at)
        print(f"Cached: {key} (TTL={effective_ttl}s)")

    def evict(self, key: str) -> bool:
        """Remove a key from cache. Returns True if key existed."""
        if key in self._store:
            del self._store[key]
            return True
        return False

    def clear(self):
        """Remove all entries from cache."""
        count = len(self._store)
        self._store.clear()
        print(f"Cache cleared: {count} entries removed")

    @property
    def size(self):
        """Return number of entries in cache (including expired)."""
        return len(self._store)
