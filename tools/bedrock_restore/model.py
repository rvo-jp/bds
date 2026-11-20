"""Explicit bounds and guarded workspace paths for offline restoration."""

import json
import math
import struct
from dataclasses import dataclass
from pathlib import Path

import numpy as np


@dataclass(frozen=True)
class Region:
    x_min: int
    x_max: int
    z_min: int
    z_max: int
    y_min: int = -64
    y_max: int = 319
    skipped: frozenset = frozenset()

    def __post_init__(self):
        if (
            self.x_min > self.x_max
            or self.z_min > self.z_max
            or (self.y_min, self.y_max) != (-64, 319)
        ):
            raise ValueError(
                "Only an ordered, full-height standard Overworld region is supported"
            )

    def contains(self, x, y, z):
        x, y, z = math.floor(x), math.floor(y), math.floor(z)
        return (
            self.x_min <= x <= self.x_max
            and self.y_min <= y <= self.y_max
            and self.z_min <= z <= self.z_max
            and (x // 16, z // 16) not in self.skipped
        )

    def chunks(self):
        for x in range(self.x_min // 16, self.x_max // 16 + 1):
            for z in range(self.z_min // 16, self.z_max // 16 + 1):
                if (x, z) not in self.skipped:
                    yield x, z

    def columns(self, cx, cz):
        x, z = np.arange(16) + cx * 16, np.arange(16) + cz * 16
        return (
            (x[:, None] >= self.x_min)
            & (x[:, None] <= self.x_max)
            & (z[None, :] >= self.z_min)
            & (z[None, :] <= self.z_max)
        )

    def mask(self, cx, cz):
        return np.repeat(self.columns(cx, cz)[:, :, None], 16, axis=2).ravel()


def chunk_prefix(cx, cz):
    return struct.pack("<ii", cx, cz)


def chunk_key(key):
    """Return coordinates, dimension and type for recognized chunk key lengths."""
    if len(key) in (9, 10):
        cx, cz = struct.unpack_from("<ii", key)
        return cx, cz, 0, key[8]
    if len(key) in (13, 14):
        cx, cz, dimension = struct.unpack_from("<iii", key)
        return cx, cz, dimension, key[12]
    return None


def guarded_root(value):
    root = Path(value).resolve()
    # A marker alone cannot authorize opening a production or snapshot database.
    if root == Path("/") or any(
        p in root.parents or p == root
        for p in (
            Path("/var/lib/bds").resolve(),
            Path("/opt/bedrock").resolve(),
            Path("/etc/bds").resolve(),
        )
    ):
        raise ValueError("Production and backup trees are forbidden")
    if (
        not (root / "source-verified.json").is_file()
        or not (root / "baseline-verified.json").is_file()
    ):
        raise ValueError("Verified independent-copy markers are required")
    return root


def copy_path(root, name):
    if name not in ("source-copy", "baseline-copy", "candidate-copy"):
        raise ValueError("Unknown workspace member")
    path = root / name
    if path.is_symlink() or path.resolve().parent != root:
        raise ValueError("Workspace member must not redirect elsewhere")
    return path


def require_regular_tree(path):
    """Refuse links and cross-copy sharing before an editable database is opened."""
    import stat

    if path.is_symlink() or not path.is_dir():
        raise ValueError("Workspace tree must be an existing, independent directory")
    for entry in path.rglob("*"):
        info = entry.lstat()
        if stat.S_ISDIR(info.st_mode):
            continue
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise ValueError(f"Non-regular or hardlinked workspace entry: {entry}")


def read_plan(path):
    plan = json.loads(Path(path).read_text())
    if set(plan) != {
        "world",
        "dimension",
        "region",
        "players",
        "excluded_entities",
        "missing_source_chunks",
    }:
        raise ValueError("Unexpected plan fields")
    if (
        plan["dimension"] != "minecraft:overworld"
        or Path(plan["world"]).name != plan["world"]
    ):
        raise ValueError("Only a named Overworld is supported")
    if not plan["world"] or plan["world"] in (".", ".."):
        raise ValueError("Invalid world name")
    if set(plan["excluded_entities"]) != {"minecraft:player", "rvo:flag"}:
        raise ValueError("Players and team flags must be excluded")
    # Player keys are private run inputs; the command never guesses identity from names.
    players = plan["players"]
    if len(players) != 2 or len({p["key"] for p in players}) != len(players):
        raise ValueError("Exactly two distinct, verified player records are required")
    for player in players:
        if set(player) != {"name", "key", "xuid"} or not player["key"].startswith(
            "player_server_"
        ):
            raise ValueError("Invalid explicit player binding")
        if not isinstance(player["xuid"], str) or not player["xuid"].isdigit():
            raise ValueError("Player XUID must be an authenticated decimal identifier")
    return plan, Region(
        **plan["region"],
        skipped=frozenset(tuple(p) for p in plan["missing_source_chunks"]),
    )
