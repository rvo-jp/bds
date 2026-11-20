"""Bedrock v8/v9 subchunks and persistent Data3D biome palettes.

Raw block order is X,Z,Y (Y varies fastest). Heightmaps use Z,X.
Unsupported encodings fail before any candidate is declared ready.
"""

import struct
from dataclasses import dataclass

import amulet_nbt as n
import numpy as np

from . import nbt

BITS = (0, 1, 2, 3, 4, 5, 6, 8, 16)


@dataclass
class Storage:
    indices: np.ndarray
    palette: list


def unpack(raw, pos, *, biome=False, previous=None):
    header = raw[pos]
    pos += 1
    if biome and header == 255:
        if previous is None:
            raise ValueError("First biome section cannot inherit a predecessor")
        return Storage(previous.indices.copy(), previous.palette.copy()), pos
    bits = header >> 1
    if bits not in BITS or bool(header & 1) != biome:
        raise ValueError(f"Unsupported palette header {header}")
    if bits:
        per = 32 // bits
        count = (4096 + per - 1) // per
        words = np.frombuffer(raw, dtype="<u4", count=count, offset=pos)
        positions = np.arange(4096)
        indices = (
            (words[positions // per] >> ((positions % per) * bits)) & ((1 << bits) - 1)
        ).astype(np.uint16)
        pos += count * 4
        (size,) = struct.unpack_from("<I", raw, pos)
        pos += 4
    else:
        indices = np.zeros(4096, dtype=np.uint16)
        size = 1
    if not 1 <= size <= 65536 or int(indices.max()) >= size:
        raise ValueError("Palette index or size out of bounds")
    if biome:
        palette = list(struct.unpack_from("<" + "I" * size, raw, pos))
        pos += size * 4
    else:
        palette = []
        for _ in range(size):
            tag, used = nbt.read(raw[pos:])
            if not isinstance(tag.tag, n.CompoundTag) or "name" not in tag.compound:
                raise ValueError("Invalid block palette entry")
            palette.append(raw[pos : pos + used])
            pos += used
    return Storage(indices, palette), pos


def pack(storage, *, biome=False):
    maximum = len(storage.palette) - 1
    bits = next(b for b in BITS if maximum < (1 << b))
    out = bytearray([bits * 2 + int(biome)])
    if bits:
        per = 32 // bits
        words = np.zeros((4096 + per - 1) // per, dtype="<u4")
        for offset in range(per):
            values = storage.indices[offset::per].astype(np.uint32)
            words[: len(values)] |= values << (offset * bits)
        out.extend(words.tobytes())
        out.extend(struct.pack("<I", len(storage.palette)))
    if biome:
        out.extend(struct.pack("<" + "I" * len(storage.palette), *storage.palette))
    else:
        out.extend(b"".join(storage.palette))
    return bytes(out)


def merge_storage(old, current, mask):
    palette = current.palette.copy()
    index = {value: i for i, value in enumerate(palette)}
    mapping = []
    for value in old.palette:
        if value not in index:
            index[value] = len(palette)
            palette.append(value)
        mapping.append(index[value])
    if len(palette) > 65536:
        raise ValueError("Merged palette too large")
    result = current.indices.copy()
    result[mask] = np.asarray(mapping, dtype=np.uint16)[old.indices[mask]]
    # Drop unused entries, including duplicate encodings; outside cells retain their value.
    used, inverse = np.unique(result, return_inverse=True)
    return Storage(inverse.astype(np.uint16), [palette[int(i)] for i in used])


def subchunk(raw, sy):
    if raw is None:
        return []
    if len(raw) < 2 or raw[0] not in (8, 9):
        raise ValueError("Only v8/v9 persistent subchunks are supported")
    pos = 2
    if raw[0] == 9:
        if struct.unpack_from("b", raw, 2)[0] != sy:
            raise ValueError("Subchunk Y does not match its key")
        pos = 3
    result = []
    for _ in range(raw[1]):
        storage, pos = unpack(raw, pos)
        result.append(storage)
    if pos != len(raw):
        raise ValueError("Trailing bytes in subchunk")
    return result


def merge_subchunk(old, current, sy, mask):
    if old == current:
        return current
    left, right = subchunk(old, sy), subchunk(current, sy)
    if not left and not right:
        return None
    reference = nbt.compound((left or right)[0].palette[0])
    if "version" not in reference:
        raise ValueError("Cannot derive air block version")
    air_nbt = nbt.dump(
        n.CompoundTag(
            {
                "name": n.StringTag("minecraft:air"),
                "states": n.CompoundTag(),
                "version": reference["version"],
            }
        )
    )
    air = Storage(np.zeros(4096, dtype=np.uint16), [air_nbt])
    layers = [
        merge_storage(
            left[i] if i < len(left) else air, right[i] if i < len(right) else air, mask
        )
        for i in range(max(len(left), len(right)))
    ]
    return (
        bytes([9, len(layers)])
        + struct.pack("b", sy)
        + b"".join(pack(x) for x in layers)
    )


def biome_sections(raw):
    pos = 512
    previous = None
    result = []
    while pos < len(raw):
        storage, pos = unpack(raw, pos, biome=True, previous=previous)
        result.append(storage)
        previous = storage
    if len(result) not in (24, 25) or pos != len(raw):
        raise ValueError(f"Unsupported Overworld biome section count: {len(result)}")
    return result


def merge_data3d(old, current, mask, columns):
    if old == current:
        return current
    if old is None or current is None or min(len(old), len(current)) < 512:
        raise ValueError("Cannot merge missing Data3D")
    heights = np.frombuffer(current[:512], "<i2").copy().reshape(16, 16)
    heights[columns.T] = np.frombuffer(old[:512], "<i2").reshape(16, 16)[columns.T]
    out = bytearray(heights.tobytes())
    left, right = biome_sections(old), biome_sections(current)
    for section, storage in enumerate(right):
        merged = (
            merge_storage(left[section], storage, mask) if section < 24 else storage
        )
        out.extend(pack(merged, biome=True))
    return bytes(out)
