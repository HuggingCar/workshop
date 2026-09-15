from pathlib import Path

import pytest

REAL_CONFIG = Path.home() / ".config" / "HuggingCar" / "Fiscal.conf"


@pytest.fixture(scope="session", autouse=True)
def tests_never_touch_the_real_config():
    """Every test must route QSettings elsewhere; a leak here would clobber the user's port."""
    before = REAL_CONFIG.read_bytes() if REAL_CONFIG.exists() else None
    yield
    after = REAL_CONFIG.read_bytes() if REAL_CONFIG.exists() else None
    assert after == before, f"tests wrote to {REAL_CONFIG}"
