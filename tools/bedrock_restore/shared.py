"""Keep shared map/tracker data current and reject newly dangling item references."""

import amulet_nbt as n

from .database import get


def check(value, source, baseline):
    if isinstance(value, n.CompoundTag):
        keys = []
        if "map_uuid" in value:
            keys.append(f"map_{int(value['map_uuid'])}".encode())
        if "trackingHandle" in value:
            keys.append(
                f"PosTrackDB-0x{int(value['trackingHandle']) & 0xFFFFFFFF:08x}".encode()
            )
        for key in keys:
            if get(source, key) is not None and get(baseline, key) is None:
                raise ValueError(
                    "Restored item needs shared data absent from the baseline"
                )
        for child in value.values():
            check(child, source, baseline)
    elif isinstance(value, n.ListTag):
        for child in value:
            check(child, source, baseline)
