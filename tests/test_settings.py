"""App settings: colour themes the user saves, and their persistence.

settings.json is a plain file people do edit by hand, so the loader has to
survive a malformed theme rather than blowing up later at paint time.
"""

from snekkie.settings import (
    DEFAULT_THEME,
    THEME_KEYS,
    THEMES,
    AppSettings,
    SettingsStore,
)


def _custom() -> AppSettings:
    return AppSettings(
        theme="Custom",
        custom_fg="#abcdef", custom_bg="#111111",
        custom_cursor="#00ff00", custom_selection="#334455",
    )


def test_saved_theme_round_trips_through_disk(tmp_path):
    store = SettingsStore(path=tmp_path / "settings.json")
    settings = _custom()
    settings.saved_themes["Lab Console"] = dict(settings.custom_colors())
    settings.theme = "Lab Console"
    store.save(settings)

    loaded = store.load()
    assert loaded.theme == "Lab Console"
    assert loaded.saved_themes["Lab Console"]["fg"] == "#abcdef"
    assert loaded.colors() == settings.custom_colors()


def test_saved_theme_is_offered_alongside_the_presets():
    settings = AppSettings()
    settings.saved_themes["Zzz Last"] = dict(THEMES[DEFAULT_THEME])
    settings.saved_themes["Aaa First"] = dict(THEMES[DEFAULT_THEME])

    names = settings.theme_names()
    assert names[:len(THEMES)] == list(THEMES), "presets should come first"
    assert names[-1] == "Custom", "Custom belongs at the end"
    # User themes sit between, in their own alphabetical order.
    assert names[len(THEMES):-1] == ["Aaa First", "Zzz Last"]


def test_custom_colours_are_kept_separate_from_a_saved_theme():
    """Selecting a saved theme must not overwrite the scratch custom colours."""
    settings = _custom()
    settings.saved_themes["Mine"] = {
        "fg": "#ff0000", "bg": "#00ff00",
        "cursor": "#0000ff", "selection": "#ffffff",
    }
    settings.theme = "Mine"

    assert settings.colors()["fg"] == "#ff0000"
    assert settings.custom_colors()["fg"] == "#abcdef"


def test_partial_saved_theme_falls_back_per_colour():
    settings = AppSettings(theme="Half")
    settings.saved_themes["Half"] = {"fg": "#123456"}

    colors = settings.colors()
    assert colors["fg"] == "#123456"
    assert set(colors) == set(THEME_KEYS)
    assert colors["bg"] == THEMES[DEFAULT_THEME]["bg"]


def test_unknown_theme_name_falls_back_to_the_default():
    assert AppSettings(theme="deleted").colors() == THEMES[DEFAULT_THEME]


def test_malformed_saved_themes_are_discarded_not_fatal():
    settings = AppSettings.from_dict({
        "theme": "ok",
        "saved_themes": {
            "ok": {"fg": "#ffffff", "bogus_key": "#000000"},
            "not-a-dict": "nope",
            "empty": {},
        },
    })
    assert list(settings.saved_themes) == ["ok"]
    assert settings.saved_themes["ok"] == {"fg": "#ffffff"}


def test_theme_renamed_by_the_rebrand_still_resolves():
    """A settings.json written as pyterm names the old default theme."""
    settings = AppSettings(theme="PyTerm Dark")

    assert settings.colors() == THEMES["Snekkie Dark"]


def test_corrupt_settings_file_degrades_to_defaults(tmp_path):
    path = tmp_path / "settings.json"
    path.write_text("{not json", encoding="utf-8")
    assert SettingsStore(path=path).load().theme == DEFAULT_THEME
