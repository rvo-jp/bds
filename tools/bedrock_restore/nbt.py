"""Lossless typed NBT handling for Bedrock's little-endian, uncompressed records."""

import amulet_nbt as n


def read(raw):
    context = n.ReadContext()
    tag = n.load(
        raw,
        compressed=False,
        little_endian=True,
        read_context=context,
        string_decoder=n.utf8_escape_decoder,
    )
    return tag, context.offset


def compound(raw):
    tag, used = read(raw)
    if used != len(raw) or tag.name or not isinstance(tag.tag, n.CompoundTag):
        raise ValueError("Expected exactly one compound NBT record")
    return tag.compound


def dump(tag):
    if not isinstance(tag, n.NamedTag):
        tag = n.NamedTag(tag)
    return tag.save_to(
        compressed=False, little_endian=True, string_encoder=n.utf8_escape_encoder
    )


def records(raw):
    """Keep each original byte slice so unselected block entities stay byte-identical."""
    pos = 0
    result = []
    while pos < len(raw):
        tag, used = read(raw[pos:])
        if used <= 0 or not isinstance(tag.tag, n.CompoundTag):
            raise ValueError("Invalid NBT stream")
        result.append((tag.compound, raw[pos : pos + used]))
        pos += used
    return result
