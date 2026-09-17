import os
import sys
from pathlib import Path

from .app import run


def main():
    data = Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local/state") / "workshop-agent"
    return run(data)


if __name__ == "__main__":
    sys.exit(main())
