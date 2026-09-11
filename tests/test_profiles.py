"""Profile storage and transport registry tests."""

import json

import pytest

from snekkie import transport
from snekkie.profiles import Profile, ProfileStore
from snekkie.transport import TransportError


@pytest.fixture
def store(tmp_path):
    return ProfileStore(tmp_path / "sessions.json")


def test_round_trip_through_disk(store):
    store.put(Profile(name="core-sw1", kind="ssh", host="10.10.0.2",
                      username="lucas", port=2222))
    reloaded = ProfileStore(store.path)
    profile = reloaded.get("core-sw1")
    assert profile is not None
    assert (profile.host, profile.username, profile.port) == ("10.10.0.2", "lucas", 2222)


def test_profiles_are_sorted_by_name(store):
    store.put(Profile(name="zulu"))
    store.put(Profile(name="alpha"))
    assert store.names() == ["alpha", "zulu"]


def test_put_overwrites_by_name(store):
    store.put(Profile(name="sw1", host="10.0.0.1"))
    store.put(Profile(name="sw1", host="10.0.0.2"))
    assert len(store.profiles) == 1
    assert store.get("sw1").host == "10.0.0.2"


def test_remove(store):
    store.put(Profile(name="temp"))
    store.remove("temp")
    assert ProfileStore(store.path).names() == []


def test_unknown_keys_in_json_are_ignored(store):
    """An old config from a future version must not crash the app."""
    store.path.write_text(json.dumps({
        "version": 1,
        "sessions": [{"name": "x", "kind": "ssh", "some_future_field": 42}],
    }))
    assert ProfileStore(store.path).get("x").kind == "ssh"


def test_corrupt_json_degrades_gracefully(store):
    store.path.write_text("{ not json at all")
    assert ProfileStore(store.path).names() == []


def test_no_secret_fields_exist():
    """Guard rail: the Profile schema must have nowhere to put a credential."""
    from dataclasses import fields

    names = {f.name for f in fields(Profile)}
    leaky = {n for n in names
             if any(s in n for s in ("password", "passphrase", "secret", "token"))}
    assert leaky == set()


def test_persisted_json_holds_no_credentials(store):
    store.put(Profile(name="sw1", kind="ssh", host="h", username="u"))
    data = json.loads(store.path.read_text())["sessions"][0]
    # 'auth' records the *method* ("password"), never the credential itself.
    assert data["auth"] == "password"
    assert set(data) == set(Profile().to_dict())


def test_copy_is_independent():
    original = Profile(name="a", host="10.0.0.1")
    clone = original.copy()
    clone.host = "10.0.0.2"
    assert original.host == "10.0.0.1"


# -- transport registry ----------------------------------------------------


def test_registry_lists_both_transports():
    assert transport.available() == {"serial": "Serial", "ssh": "SSH"}


def test_factory_builds_the_right_class():
    ssh = transport.create(Profile(kind="ssh", host="h", username="u"))
    serial = transport.create(Profile(kind="serial", device="COM3"))
    assert type(ssh).__name__ == "SSHTransport"
    assert type(serial).__name__ == "SerialTransport"


def test_unknown_kind_raises():
    with pytest.raises(TransportError):
        transport.create(Profile(kind="telnet"))


def test_descriptions_are_readable():
    ssh = transport.create(Profile(kind="ssh", host="sw1", username="lucas", port=2222))
    serial = transport.create(Profile(kind="serial", device="COM3", baud=9600))
    assert ssh.description == "SSH  lucas@sw1:2222"
    assert serial.description == "Serial  COM3  9600-8N1"


def test_serial_rejects_break_only_when_disconnected():
    serial = transport.create(Profile(kind="serial", device="COM3"))
    with pytest.raises(TransportError):
        serial.send_break()


def test_ssh_does_not_support_break():
    ssh = transport.create(Profile(kind="ssh", host="h"))
    with pytest.raises(TransportError):
        ssh.send_break()
