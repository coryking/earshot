import pytest


@pytest.fixture(autouse=True)
def never_rewrap_under_radio_borrow(monkeypatch):
    """A test must never replace the pytest process with radio-borrow (and stop a live service)."""
    monkeypatch.setenv("PLUTO_RADIO_BORROWED", "1")
