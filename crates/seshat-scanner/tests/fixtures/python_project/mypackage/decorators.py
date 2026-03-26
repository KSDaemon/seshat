"""Decorators module — tests decorator extraction."""

import functools
from typing import Callable, TypeVar

T = TypeVar("T")


def retry(max_attempts: int = 3) -> Callable:
    """Retry decorator."""

    def decorator(func):
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            for attempt in range(max_attempts):
                try:
                    return func(*args, **kwargs)
                except Exception:
                    if attempt == max_attempts - 1:
                        raise

        return wrapper

    return decorator


def deprecated(message: str) -> Callable:
    """Mark a function as deprecated."""

    def decorator(func):
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            import warnings

            warnings.warn(f"{func.__name__}: {message}", DeprecationWarning)
            return func(*args, **kwargs)

        return wrapper

    return decorator


@deprecated("Use new_handler instead")
def old_handler():
    pass


@retry(max_attempts=5)
async def flaky_operation() -> bool:
    pass
