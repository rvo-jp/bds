"""Construct a candidate from an offline baseline; never install it into production."""

import copy
import hashlib
import json
import shutil
import struct
import time
import uuid

import amulet_nbt as n

from . import nbt, palettes, shared, validation
from .database import get, opened, prefix_items
from .entities import plan_actors
from .filesystem import save_json, sync_tree, workspace_lock
from .model import (
    Region,
    chunk_key,
    chunk_prefix,
    copy_path,
    guarded_root,
    read_plan,
    require_regular_tree,
)

INVENTORIES = ("Inventory", "Armor", "Offhand", "EnderChestInventory")
KNOWN_TAGS = {0x2B, 0x2C, 0x2F, 0x31, 0x33, 0x35, 0x36, 0x3A, 0x3F, 0x40, 0x41, 0x77}
# Keep format/generation metadata bound to the current global dictionaries.
# In particular, MetaDataHash is an indirection, not a terrain checksum to copy.
CURRENT_METADATA = {0x2C, 0x36, 0x3F, 0x40, 0x41, 0x77}


def position(data):
    if not {"x", "y", "z"} <= set(data):
        raise ValueError("Positional record has no complete coordinates")
    return tuple(int(data[key]) for key in ("x", "y", "z"))


def merge_block_entities(old, current, region):
    left, right = nbt.records(old or b""), nbt.records(current or b"")

    def crossing_pairs(records):
        pairs = set()
        for data, _ in records:
            x, y, z = position(data)
            if region.contains(x, y, z) and ("pairx" in data or "pairz" in data):
                if not {"pairx", "pairz"} <= set(data):
                    raise ValueError("Incomplete paired-container metadata")
                partner = (int(data["pairx"]), y, int(data["pairz"]))
                if not region.contains(*partner):
                    pairs.add(
                        ((x, y, z), str(data["id"]), partner, str(data.get("pairlead")))
                    )
        return pairs

    if crossing_pairs(left) != crossing_pairs(right):
        raise ValueError(
            "A paired container crosses the restoration boundary with different pairing"
        )
    return (
        b"".join(raw for data, raw in right if not region.contains(*position(data)))
        + b"".join(raw for data, raw in left if region.contains(*position(data)))
    ) or None


def merge_ticks(old, current, region):
    if old == current:
        return current
    left = nbt.compound(old) if old else None
    right = nbt.compound(current) if current else None
    for value in (left, right):
        if value is not None and set(value) != {"currentTick", "tickList"}:
            raise ValueError("Unsupported scheduled-tick record")
        if value is not None and (
            not isinstance(value["currentTick"], n.IntTag)
            or int(value["currentTick"]) < 0
        ):
            raise ValueError("Wrapped or unsupported scheduled-tick clock")
    base = int((right if right is not None else left)["currentTick"])
    result = []
    if right is not None:
        result.extend(t for t in right["tickList"] if not region.contains(*position(t)))
    if left is not None:
        delta = base - int(left["currentTick"])
        for tick in left["tickList"]:
            if not region.contains(*position(tick)):
                continue
            tick = copy.deepcopy(tick)
            if "time" not in tick or not isinstance(tick["time"], n.LongTag):
                raise ValueError("Unknown scheduled-tick timestamp encoding")
            if int(tick["time"]) >= 0:
                tick["time"] = n.LongTag(int(tick["time"]) + delta)
            result.append(tick)
    if not result:
        return None
    template = right if right is not None else left
    return nbt.dump(
        n.CompoundTag(
            {
                "currentTick": copy.deepcopy(template["currentTick"]),
                "tickList": n.ListTag(result, 10),
            }
        )
    )


def plan_chunks(source, baseline, region):
    full_region = Region(region.x_min, region.x_max, region.z_min, region.z_max)
    missing = {
        coords
        for coords in full_region.chunks()
        if get(source, chunk_prefix(*coords) + b"\x2b") is None
    }
    if missing != set(region.skipped):
        raise ValueError(f"Missing-source policy must exactly match: {sorted(missing)}")
    patch = {}
    report = {
        "chunks": 0,
        "boundary_chunks": 0,
        "skipped_source_chunks": sorted(missing),
    }
    for cx, cz in region.chunks():
        prefix = chunk_prefix(cx, cz)
        old = {
            k[len(prefix) :]: v
            for k, v in prefix_items(source, prefix)
            if len(k) in (9, 10) and chunk_key(k)[2] == 0
        }
        current = {
            k[len(prefix) :]: v
            for k, v in prefix_items(baseline, prefix)
            if len(k) in (9, 10) and chunk_key(k)[2] == 0
        }
        if b"\x2b" not in current or any(
            get(db, prefix + b"\x36") != struct.pack("<i", 2)
            for db in (source, baseline)
        ):
            raise ValueError(
                f"Fully generated source and baseline required at {cx},{cz}"
            )
        if old.get(b"\x2c") != current.get(b"\x2c"):
            raise ValueError(
                "Chunk versions differ; conversion must be separately validated"
            )
        columns = region.columns(cx, cz)
        full = bool(columns.all())
        mask = region.mask(cx, cz)
        report["chunks"] += 1
        report["boundary_chunks"] += not full
        for suffix in old.keys() | current.keys():
            tag = suffix[0]
            left, right = old.get(suffix), current.get(suffix)
            if left == right:
                continue
            if tag not in KNOWN_TAGS:
                raise ValueError(
                    f"Changing unsupported chunk tag 0x{tag:02x} at {cx},{cz} (source={len(left) if left else 0}, baseline={len(right) if right else 0})"
                )
            if tag in CURRENT_METADATA:
                continue
            if tag == 0x2F:
                if len(suffix) != 2:
                    raise ValueError("Invalid subchunk key")
                sy = struct.unpack("b", suffix[1:])[0]
                if not -4 <= sy < 20:
                    raise ValueError(
                        "Unexpected subchunk outside standard Overworld height"
                    )
                # Validate both, even when an interior chunk can be copied unchanged.
                palettes.subchunk(left, sy)
                palettes.subchunk(right, sy)
                value = left if full else palettes.merge_subchunk(left, right, sy, mask)
                validation.subchunk(left, right, value, sy, mask)
            elif tag == 0x31:
                for data, _ in nbt.records(left or b""):
                    if region.contains(*position(data)):
                        shared.check(data, source, baseline)
                value = merge_block_entities(left, right, region)
                validation.block_entities(left, right, value, region)
            elif tag in (0x33, 0x3A):
                value = merge_ticks(left, right, region)
            elif tag == 0x2B:
                value = palettes.merge_data3d(left, right, mask, columns)
                validation.data3d(left, right, value, mask, columns)
            elif full:
                value = left
            else:
                raise ValueError(
                    f"Boundary chunk has changing unsupported metadata 0x{tag:02x} at {cx},{cz}"
                )
            if value != right:
                patch[prefix + suffix] = value
    return patch, report


def plan_players(source, baseline, players):
    patch = {}
    report = []
    for player in players:
        identity = str(
            uuid.UUID(
                bytes=hashlib.md5(
                    ("pocket-auth-1-xuid:" + player["xuid"]).encode()
                ).digest(),
                version=3,
            )
        )
        for db in (source, baseline):
            binding_raw = get(db, ("player_" + identity).encode())
            if binding_raw is None:
                raise ValueError("Authenticated player identity missing")
            binding = nbt.compound(binding_raw)
            if (
                str(binding.get("MsaId")) != identity
                or str(binding.get("ServerId")) != player["key"]
            ):
                raise ValueError(
                    "Authenticated player binding differs between plan and database"
                )
        key = player["key"].encode("ascii")
        old_raw, current_raw = get(source, key), get(baseline, key)
        if old_raw is None or current_raw is None:
            raise ValueError(f"Player record absent: {player['name']}")
        left, right = nbt.compound(old_raw), nbt.compound(current_raw)
        merged = copy.deepcopy(right)
        for field in INVENTORIES:
            if field not in left or not isinstance(left[field], n.ListTag):
                raise ValueError(f"Inventory field missing/unsupported: {field}")
            merged[field] = copy.deepcopy(left[field])
            shared.check(left[field], source, baseline)
        # Mainhand duplicates the selected hotbar item. Keep the current selection,
        # but derive this cache from the restored inventory instead of retaining stale gear.
        selected = int(right["SelectedInventorySlot"])
        if not 0 <= selected < 9 or len(left["Inventory"]) != 36:
            raise ValueError("Unsupported player inventory layout")
        slots = {int(item["Slot"]): item for item in left["Inventory"]}
        if set(slots) != set(range(36)):
            raise ValueError("Duplicate or missing player inventory slots")
        hand = copy.deepcopy(slots[selected])
        del hand["Slot"]
        if not int(hand.get("Count", n.ByteTag(0))):
            hand = n.CompoundTag()
        merged["Mainhand"] = n.ListTag([hand], 10)
        for field in set(right) | set(merged):
            if field == "Mainhand":
                continue
            expected = left.get(field) if field in INVENTORIES else right.get(field)
            if merged.get(field) != expected:
                raise ValueError("Unexpected player-data change")
        patch[key] = nbt.dump(merged)
        report.append(
            {
                "name": player["name"],
                "key": player["key"],
                "fields": {field: len(left[field]) for field in INVENTORIES},
            }
        )
    return patch, report


def digest_except(db, excluded):
    result = hashlib.sha256()
    count = 0
    for key, value in db.items():
        if key in excluded:
            continue
        result.update(
            struct.pack("<Q", len(key)) + key + struct.pack("<Q", len(value)) + value
        )
        count += 1
    return {"sha256": result.hexdigest(), "records": count}


def build(workspace, plan_path):
    root = guarded_root(workspace)
    with workspace_lock(root):
        return build_locked(root, plan_path)


def build_locked(root, plan_path):
    started = time.monotonic()
    plan, region = read_plan(plan_path)
    source_data = copy_path(root, "source-copy")
    require_regular_tree(source_data)
    source = source_data / "worlds" / plan["world"] / "db"
    baseline_data = copy_path(root, "baseline-copy")
    require_regular_tree(baseline_data)
    baseline = baseline_data / "worlds" / plan["world"] / "db"
    candidate_data = copy_path(root, "candidate-copy")
    if candidate_data.exists():
        raise ValueError(
            "Candidate already exists; retain or remove it explicitly before rebuilding"
        )
    if (root / "restore-report.json").exists():
        raise ValueError(
            "Workspace already has a verification report; use a fresh workspace"
        )
    print("Planning chunk changes", flush=True)
    with opened(source) as old, opened(baseline) as current:
        patch, chunks = plan_chunks(old, current, region)
        print("Planning inventory changes", flush=True)
        players_patch, players = plan_players(old, current, plan["players"])
        print("Planning actor changes", flush=True)
        actors_patch, actors = plan_actors(
            old, current, region, set(plan["excluded_entities"])
        )
        patch.update(players_patch)
        patch.update(actors_patch)
        patch = {k: v for k, v in patch.items() if get(current, k) != v}
        print(f"Verifying unchanged data around {len(patch)} changed keys", flush=True)
        outside = digest_except(current, set(patch))
        changes = [
            {
                "key": k.hex(),
                "before": hashlib.sha256(get(current, k)).hexdigest()
                if get(current, k) is not None
                else None,
                "after": hashlib.sha256(v).hexdigest() if v is not None else None,
            }
            for k, v in sorted(patch.items())
        ]
    require_regular_tree(baseline_data)
    shutil.copytree(baseline_data, candidate_data)
    require_regular_tree(candidate_data)
    candidate = candidate_data / "worlds" / plan["world"] / "db"
    with opened(candidate) as db:
        updates = {k: v for k, v in patch.items() if v is not None}
        db.putBatch(updates)
        for key, value in patch.items():
            if value is None:
                db.delete(key)
    print("Reading candidate back", flush=True)
    with opened(candidate) as db:
        for key, value in patch.items():
            if get(db, key) != value:
                raise ValueError("Candidate readback failed")
        if digest_except(db, set(patch)) != outside:
            raise ValueError("Unselected logical data changed")
    sync_tree(candidate_data)
    with open(plan_path, "rb") as handle:
        plan_sha256 = hashlib.sha256(handle.read()).hexdigest()
    report = {
        "plan": plan,
        "plan_sha256": plan_sha256,
        "source_snapshot": json.loads((root / "source-verified.json").read_text())[
            "id"
        ],
        "baseline_snapshot": json.loads((root / "baseline-verified.json").read_text())[
            "id"
        ],
        "chunks": chunks,
        "players": players,
        "actors": actors,
        "changed_keys": len(patch),
        "outside": outside,
        "changes": changes,
        "seconds": round(time.monotonic() - started, 2),
        "status": "offline-verified; isolated BDS rehearsal still required",
    }
    report_path = root / "restore-report.json"
    save_json(report_path, report)
    print(
        json.dumps(
            {k: v for k, v in report.items() if k not in ("changes", "plan")},
            ensure_ascii=False,
        ),
        flush=True,
    )
    return report
