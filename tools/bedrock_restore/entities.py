"""Restore actors and their digests together while preserving excluded actors."""

import math
import struct
from dataclasses import dataclass

from . import nbt, shared
from .database import get, prefix_items


@dataclass
class Actor:
    key: bytes
    raw: object
    uid: int
    kind: str
    pos: tuple
    links: frozenset
    digest: bytes
    dimension: int

    def selected(self, region, excluded):
        return (
            self.dimension == 0
            and self.kind not in excluded
            and region.contains(*self.pos)
        )


def registry(db, region, excluded):
    digests = {}
    membership = {}
    for key, raw in prefix_items(db, b"digp"):
        suffix = key[4:]
        if len(suffix) not in (8, 12) or len(raw) % 8:
            raise ValueError("Unsupported actor digest format")
        dimension = struct.unpack_from("<i", suffix, 8)[0] if len(suffix) == 12 else 0
        ids = [raw[i : i + 8] for i in range(0, len(raw), 8)]
        digests[key] = ids
        for ident in ids:
            membership.setdefault(ident, []).append((key, dimension))
    actors = {}
    uids = {}
    for key, raw in prefix_items(db, b"actorprefix"):
        ident = key[len(b"actorprefix") :]
        if len(ident) != 8:
            raise ValueError("Unsupported actor storage ID")
        if ident not in membership:
            # Keep orphan records intact. They must not be resurrected as active actors.
            continue
        data = nbt.compound(raw)
        refs = membership[ident]
        dimensions = {dim for _, dim in refs}
        dimension = next(iter(dimensions)) if len(dimensions) == 1 else -1
        if not {"UniqueID", "identifier", "Pos"} <= set(data):
            raise ValueError("Actor identity/position missing")
        pos = tuple(float(v) for v in data["Pos"])
        kind = str(data["identifier"])
        if len(pos) != 3 or not all(math.isfinite(v) for v in pos):
            raise ValueError("Invalid actor position")
        if dimension == -1 and region.contains(*pos):
            raise ValueError("Actor dimension is ambiguous inside the selected region")
        selected = dimension == 0 and kind not in excluded and region.contains(*pos)
        matching = [
            key
            for key, _ in refs
            if struct.unpack_from("<ii", key, 4)
            == (math.floor(pos[0]) // 16, math.floor(pos[2]) // 16)
        ]
        digest = matching[0] if matching else refs[0][0]
        if selected and (len(refs) != 1 or len(matching) != 1):
            raise ValueError("Selected actor has stale/ambiguous chunk references")
        actor = Actor(
            key,
            raw if selected else None,
            int(data["UniqueID"]),
            kind,
            pos,
            links(data),
            digest,
            dimension,
        )
        uids.setdefault(actor.uid, []).append(actor)
        actors[key] = actor
        cx, cz = struct.unpack_from("<ii", digest, 4)
        if selected and (math.floor(pos[0]) // 16, math.floor(pos[2]) // 16) != (
            cx,
            cz,
        ):
            raise ValueError("Actor position disagrees with its chunk digest")
        if selected and "DimensionId" in data and int(data["DimensionId"]) != dimension:
            raise ValueError("Actor dimension disagrees with its chunk digest")
    absent = set(membership) - {key[len(b"actorprefix") :] for key in actors}
    selected_chunks = set(region.chunks())
    for ident in absent:
        if any(
            dim == 0 and struct.unpack_from("<ii", key, 4) in selected_chunks
            for key, dim in membership[ident]
        ):
            raise ValueError("Selected chunk digest references a missing actor")
    return actors, digests, uids


def links(data):
    result = set()
    for link in data.get("LinksTag", ()):
        if "entityID" not in link:
            raise ValueError("Unknown actor rider link encoding")
        result.add(int(link["entityID"]))
    leash = data.get("LeasherID")
    if leash is not None and int(leash) not in (-1, 0):
        result.add(int(leash))
    return frozenset(result)


def plan_actors(source, baseline, region, excluded):
    old, _, old_uids = registry(source, region, excluded)
    current, digests, uids = registry(baseline, region, excluded)
    remove = {k: a for k, a in current.items() if a.selected(region, excluded)}
    restore = {k: a for k, a in old.items() if a.selected(region, excluded)}
    preserved_outside = []
    changed_owners = set()
    for key, raw in prefix_items(source, b"player_server_"):
        player = nbt.compound(raw)
        current_raw = get(baseline, key)
        current_player = nbt.compound(current_raw) if current_raw is not None else {}
        if (
            "UniqueID" in player
            and current_player.get("UniqueID") != player["UniqueID"]
        ):
            changed_owners.add(int(player["UniqueID"]))
    for key, actor in list(restore.items()):
        data = nbt.compound(actor.raw)
        if any(
            int(data[field]) in changed_owners
            for field in ("OwnerNew", "OwnerID")
            if field in data
        ):
            raise ValueError("Restored ownership refers to a changed player identity")
        if len(old_uids[actor.uid]) != 1:
            raise ValueError("Selected source UniqueID is not unique")
        matches = uids.get(actor.uid, [])
        if len(matches) > 1:
            raise ValueError("Selected actor identity is ambiguous in the baseline")
        other = matches[0] if matches else None
        if other is not None and other.key != key:
            raise ValueError("Actor UniqueID now belongs to another storage key")
        if key in current and key not in remove:
            if current[key].uid != actor.uid:
                raise ValueError("Actor storage ID collision")
            preserved_outside.append({"key": key.hex(), "uid": actor.uid})
            del restore[key]
    removed_uids = {a.uid for a in remove.values()}
    restored_uids = {a.uid for a in restore.values()}
    for actor in restore.values():
        shared.check(nbt.compound(actor.raw), source, baseline)
    # Crossing a scope boundary must never silently detach an outside rider/leash.
    for actor in current.values():
        if actor.key not in remove and actor.links & removed_uids:
            raise ValueError(
                "Outside actor links to a replaced actor; explicit resolution required"
            )
    for actor in remove.values():
        if actor.links - removed_uids:
            raise ValueError("Replaced actor links outside the region")
    for actor in restore.values():
        if actor.links - restored_uids:
            raise ValueError("Restored rider/leash links outside the restoration set")
    changes = {key: None for key in remove}
    changes.update({key: a.raw for key, a in restore.items()})
    touched = {a.digest for a in remove.values()} | {a.digest for a in restore.values()}
    for digest in touched:
        ids = [
            ident
            for ident in digests.get(digest, [])
            if b"actorprefix" + ident not in remove
        ]
        ids.extend(
            a.key[len(b"actorprefix") :] for a in restore.values() if a.digest == digest
        )
        if len(ids) != len(set(ids)):
            raise ValueError("Duplicate planned actor digest")
        changes[digest] = b"".join(ids) if ids else None
    return changes, {
        "removed": len(remove),
        "restored": len(restore),
        "outside_individuals_preserved": preserved_outside,
    }
