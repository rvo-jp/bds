"""Check coordinate-level preservation independently of the merging operations."""

import numpy as np

from . import nbt, palettes


def dense_blocks(raw, sy):
    layers = []
    for storage in palettes.subchunk(raw, sy):
        values = []
        for raw_entry in storage.palette:
            entry = nbt.compound(raw_entry)
            is_air = str(entry["name"]) == "minecraft:air" and not len(entry["states"])
            values.append(b"<implicit air>" if is_air else raw_entry)
        palette = np.empty(len(values), dtype=object)
        palette[:] = values
        layers.append(palette[storage.indices])
    return layers


def subchunk(old, current, result, sy, mask):
    left, right, actual = [dense_blocks(raw, sy) for raw in (old, current, result)]
    air = np.full(4096, b"<implicit air>", dtype=object)
    for i in range(max(len(left), len(right), len(actual))):
        want = (right[i] if i < len(right) else air).copy()
        want[mask] = (left[i] if i < len(left) else air)[mask]
        got = actual[i] if i < len(actual) else air
        if not np.array_equal(want, got):
            raise ValueError("Coordinate-level block/layer verification failed")


def data3d(old, current, result, mask, columns):
    before = np.frombuffer(current[:512], "<i2").reshape(16, 16)
    old_height = np.frombuffer(old[:512], "<i2").reshape(16, 16)
    actual = np.frombuffer(result[:512], "<i2").reshape(16, 16)
    if not np.array_equal(actual[~columns.T], before[~columns.T]) or not np.array_equal(
        actual[columns.T], old_height[columns.T]
    ):
        raise ValueError("Heightmap coordinate verification failed")
    groups = [palettes.biome_sections(raw) for raw in (old, current, result)]
    if len(groups[1]) != len(groups[2]):
        raise ValueError("Unexpected change to biome coverage")
    for section in range(len(groups[1])):
        arrays = [
            np.asarray(group[section].palette, dtype=np.uint32)[group[section].indices]
            if section < len(group)
            else None
            for group in groups
        ]
        want = arrays[1].copy()
        if section < 24:
            want[mask] = arrays[0][mask]
        if not np.array_equal(want, arrays[2]):
            raise ValueError("Biome coordinate verification failed")


def block_entities(old, current, result, region):
    def values(raw):
        indexed = {}
        for data, entry in nbt.records(raw or b""):
            pos = tuple(int(data[key]) for key in ("x", "y", "z"))
            if pos in indexed:
                raise ValueError("Duplicate block entities at one position")
            indexed[pos] = entry
        return indexed

    left, right, actual = [values(raw) for raw in (old, current, result)]
    wanted = {p: v for p, v in right.items() if not region.contains(*p)}
    wanted.update({p: v for p, v in left.items() if region.contains(*p)})
    if actual != wanted:
        raise ValueError("Block-entity selection verification failed")
