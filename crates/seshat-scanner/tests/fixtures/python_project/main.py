"""Application entry point."""

import sys
from pathlib import Path

from mypackage import User, Config, UserService
from mypackage.utils import format_name


def main() -> int:
    config = Config(host="0.0.0.0", port=3000, debug=True)
    service = UserService(config)

    user = User(
        name=format_name("John", "Doe"),
        email="john@example.com",
    )
    service.add_user(user)

    found = service.find_user("John Doe")
    if found:
        print(found.display_name())

    return 0


if __name__ == "__main__":
    sys.exit(main())
