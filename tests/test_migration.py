"""Carrying an existing install's config across the rename to Snekkie.

The app used to store sessions and settings under a directory named after
its old name. Without a migration the rename presents as every saved
session having vanished, which is the worst possible way to ship a rebrand.
"""

import json

import pytest

from snekkie import profiles
from snekkie.profiles import LEGACY_DIR_NAME, ProfileStore, migrate_legacy_config
from snekkie.settings import SettingsStore


@pytest.fixture
def config_home(tmp_path, monkeypatch):
    """Point config_dir() at a scratch directory on any platform."""
    monkeypatch.setattr(profiles, "config_dir", lambda: tmp_path / "snekkie")
    return tmp_path


def _write_legacy(root, sessions=("core-switch",), theme="PyTerm Dark"):
    old = root / LEGACY_DIR_NAME
    old.mkdir(parents=True, exist_ok=True)
    (old / "sessions.json").write_text(json.dumps({
        "version": 1,
        "sessions": [{"name": name, "host": "10.0.0.1"} for name in sessions],
    }), encoding="utf-8")
    (old / "settings.json").write_text(
        json.dumps({"theme": theme, "font_family": "8514oem"}), encoding="utf-8")
    return old


def test_saved_sessions_survive_the_rename(config_home):
    _write_legacy(config_home, sessions=("core-switch", "lab-router"))

    migrate_legacy_config()

    store = ProfileStore(profiles.config_dir() / "sessions.json")
    assert store.names() == ["core-switch", "lab-router"]


def test_settings_survive_the_rename(config_home):
    _write_legacy(config_home)

    migrate_legacy_config()

    settings = SettingsStore(profiles.config_dir() / "settings.json").load()
    assert settings.font_family == "8514oem"


def test_migration_leaves_the_originals_alone(config_home):
    old = _write_legacy(config_home)

    migrate_legacy_config()

    # Copied, not moved: if anything goes wrong the originals are still there.
    assert (old / "sessions.json").exists()
    assert (old / "settings.json").exists()


def test_migration_never_overwrites_newer_config(config_home):
    _write_legacy(config_home)
    new = profiles.config_dir()
    new.mkdir(parents=True, exist_ok=True)
    (new / "settings.json").write_text(
        json.dumps({"font_family": "Cascadia Mono"}), encoding="utf-8")

    migrate_legacy_config()

    settings = SettingsStore(new / "settings.json").load()
    assert settings.font_family == "Cascadia Mono"


def test_migration_is_a_no_op_without_a_legacy_directory(config_home):
    migrate_legacy_config()  # must not raise

    assert not (profiles.config_dir() / "sessions.json").exists()


def test_migration_runs_twice_without_duplicating(config_home):
    _write_legacy(config_home, sessions=("core-switch",))

    migrate_legacy_config()
    migrate_legacy_config()

    store = ProfileStore(profiles.config_dir() / "sessions.json")
    assert store.names() == ["core-switch"]
