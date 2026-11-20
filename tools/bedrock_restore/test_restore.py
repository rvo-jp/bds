"""Destructive-edit boundaries and preservation tests on synthetic offline data."""

import hashlib
import json
import struct
import tempfile
import unittest
import uuid
from pathlib import Path

import amulet_nbt as n
import numpy as np

from tools.bedrock_restore import nbt, palettes, shared
from tools.bedrock_restore.entities import plan_actors, registry
from tools.bedrock_restore.filesystem import workspace_lock
from tools.bedrock_restore.model import (
    Region,
    chunk_prefix,
    guarded_root,
    require_regular_tree,
)
from tools.bedrock_restore.restore import (
    build,
    merge_block_entities,
    merge_ticks,
    plan_chunks,
    plan_players,
)
from tools.bedrock_restore.safety import validate_stop
from tools.bedrock_restore.workspace import seed


def block(name):
    return nbt.dump(
        n.CompoundTag(
            {
                "name": n.StringTag(name),
                "states": n.CompoundTag(),
                "version": n.IntTag(18168865),
            }
        )
    )


def entity(x, y, z, kind="Chest", **kwargs):
    data = n.CompoundTag(
        {"id": n.StringTag(kind), "x": n.IntTag(x), "y": n.IntTag(y), "z": n.IntTag(z)}
    )
    data.update(kwargs)
    return nbt.dump(data)


class DB(dict):
    def iterate(self, start, end):
        return ((k, v) for k, v in sorted(self.items()) if start <= k <= end)


def actor(db, ident, x, z, kind="minecraft:cow", dimension=0, links=()):
    raw_id = struct.pack(">Q", ident)
    key = b"actorprefix" + raw_id
    data = n.CompoundTag(
        {
            "UniqueID": n.LongTag(-ident),
            "identifier": n.StringTag(kind),
            "Pos": n.ListTag([n.FloatTag(x), n.FloatTag(65), n.FloatTag(z)], 5),
        }
    )
    if links:
        data["LinksTag"] = n.ListTag(
            [n.CompoundTag({"entityID": n.LongTag(i)}) for i in links], 10
        )
    db[key] = nbt.dump(data)
    digest = b"digp" + chunk_prefix(int(np.floor(x)) // 16, int(np.floor(z)) // 16)
    if dimension:
        digest += struct.pack("<i", dimension)
    db[digest] = db.get(digest, b"") + raw_id
    return key


class Boundaries(unittest.TestCase):
    def test_workspace_excludes_simultaneous_build_or_rehearsal(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            with workspace_lock(root):
                with self.assertRaises(BlockingIOError):
                    with workspace_lock(root):
                        pass

    def test_negative_floor_and_inclusive_block_columns(self):
        r = Region(-2692, -1892, 533, 1333)
        self.assertEqual(2601, len(list(r.chunks())))
        self.assertTrue(r.contains(-1891.01, 65, 1333.99))
        self.assertFalse(r.contains(-1891, 65, 1333))
        self.assertFalse(r.contains(-2692.01, 65, 533))
        self.assertEqual(801 * 801, sum(int(r.columns(*p).sum()) for p in r.chunks()))

    def test_missing_chunk_is_excluded_for_blocks_and_actors(self):
        r = Region(0, 31, 0, 31, skipped=frozenset({(0, 0)}))
        self.assertFalse(r.contains(1, 65, 1))
        self.assertEqual(3, len(list(r.chunks())))

    def test_workspaces_refuse_links_and_production(self):
        for value in ["/var/lib/bds", "/var/lib/bds/backups", "/etc/bds"]:
            with self.assertRaises(ValueError):
                guarded_root(value)
        with tempfile.TemporaryDirectory() as name:
            p = Path(name)
            (p / "a").write_bytes(b"world")
            (p / "b").symlink_to("a")
            with self.assertRaises(ValueError):
                require_regular_tree(p)
            (p / "b").unlink()
            (p / "b").hardlink_to(p / "a")
            with self.assertRaises(ValueError):
                require_regular_tree(p)

    def test_safety_capture_requires_clean_exit_and_all_operators_stopped(self):
        mode = {"phase": "ready"}
        status = {
            "stopped": True,
            "failed": False,
            "child_exit": 0,
            "boot": "this-boot",
            "invocation": "saved",
        }
        units = [
            ("bds.service", "inactive", "0"),
            ("bds-update.timer", "inactive", "0"),
        ]
        validate_stop(mode, status, "this-boot", units)
        for change in [
            {"stopped": False},
            {"failed": True},
            {"child_exit": -9},
            {"boot": "previous-boot"},
        ]:
            with self.assertRaises(ValueError):
                validate_stop(mode, {**status, **change}, "this-boot", units)
        with self.assertRaises(ValueError):
            validate_stop(mode, status, "this-boot", [("bds.service", "active", "123")])
        with self.assertRaises(ValueError):
            validate_stop({"phase": "stopping"}, status, "this-boot", units)


class PaletteFormats(unittest.TestCase):
    def test_native_word_layout_with_non_power_of_two_padding(self):
        # Three-bit values: ten cells fit per word, leaving two padding bits.
        values = [0, 1, 2, 3, 4, 5, 6, 7, 1, 2] + [0] * 4086
        words = [
            sum(values[j + i] << (3 * i) for i in range(min(10, 4096 - j)))
            for j in range(0, 4096, 10)
        ]
        raw = (
            b"\x06"
            + struct.pack("<" + "I" * len(words), *words)
            + struct.pack("<I", 8)
            + b"".join(block(str(i)) for i in range(8))
        )
        decoded, end = palettes.unpack(raw, 0)
        self.assertEqual(len(raw), end)
        self.assertEqual(values, decoded.indices.tolist())

    def test_palette_sizes_and_roundtrips(self):
        for size in [1, 2, 3, 5, 9, 17, 33, 65, 257]:
            with self.subTest(size=size):
                indices = (np.arange(4096) % size).astype(np.uint16)
                storage = palettes.Storage(
                    indices, [block("b" + str(i)) for i in range(size)]
                )
                raw = palettes.pack(storage)
                decoded, end = palettes.unpack(raw, 0)
                self.assertEqual(len(raw), end)
                self.assertTrue(np.array_equal(indices, decoded.indices))
                self.assertEqual(storage.palette, decoded.palette)

    def test_boundary_merge_keeps_outside_and_secondary_water_layer(self):
        a = palettes.Storage(np.zeros(4096, np.uint16), [block("old")])
        b = palettes.Storage(np.zeros(4096, np.uint16), [block("current")])
        water = palettes.Storage(np.zeros(4096, np.uint16), [block("minecraft:water")])
        old = b"\x09\x02\xfc" + palettes.pack(a) + palettes.pack(water)
        current = b"\x09\x01\xfc" + palettes.pack(b)
        r = Region(-14, -10, 2, 7)
        mask = r.mask(-1, 0)
        merged = palettes.subchunk(palettes.merge_subchunk(old, current, -4, mask), -4)
        for x, z, y in [(2, 2, 0), (6, 7, 15), (1, 2, 0), (6, 8, 0)]:
            offset = x * 256 + z * 16 + y
            inside = 2 <= x <= 6 and 2 <= z <= 7
            self.assertEqual(
                "old" if inside else "current",
                str(
                    nbt.compound(merged[0].palette[int(merged[0].indices[offset])])[
                        "name"
                    ]
                ),
            )
            self.assertEqual(
                "minecraft:water" if inside else "minecraft:air",
                str(
                    nbt.compound(merged[1].palette[int(merged[1].indices[offset])])[
                        "name"
                    ]
                ),
            )

    def test_data3d_height_axis_and_biomes_outside_preserved(self):
        old_height = np.arange(256, dtype="<i2").reshape(16, 16)
        now_height = np.full((16, 16), -7, dtype="<i2")
        old = old_height.tobytes() + b"\x01" + struct.pack("<I", 42) + b"\xff" * 24
        now = now_height.tobytes() + b"\x01" + struct.pack("<I", 7) + b"\xff" * 24
        r = Region(3, 5, 8, 9)
        merged = palettes.merge_data3d(old, now, r.mask(0, 0), r.columns(0, 0))
        heights = np.frombuffer(merged[:512], "<i2").reshape(16, 16)
        self.assertEqual(8 * 16 + 3, int(heights[8, 3]))
        self.assertEqual(-7, int(heights[3, 8]))
        pos = 512
        previous = None
        for section in range(25):
            data, pos = palettes.unpack(merged, pos, biome=True, previous=previous)
            self.assertEqual(
                42 if section < 24 else 7,
                data.palette[int(data.indices[3 * 256 + 8 * 16])],
            )
            self.assertEqual(7, data.palette[int(data.indices[0])])
            previous = data
        self.assertEqual(len(merged), pos)

    def test_unknown_palette_header_and_y_mismatch_fail(self):
        with self.assertRaises(ValueError):
            palettes.unpack(b"\x0e", 0)
        with self.assertRaises(ValueError):
            palettes.subchunk(b"\x09\x00\x00", 1)

    def test_24_section_source_preserves_target_upper_biome_section(self):
        old = bytes(512) + b"\x01" + struct.pack("<I", 42) + b"\xff" * 23
        now = bytes(512) + b"\x01" + struct.pack("<I", 7) + b"\xff" * 24
        r = Region(0, 15, 0, 15)
        merged = palettes.merge_data3d(old, now, r.mask(0, 0), r.columns(0, 0))
        sections = palettes.biome_sections(merged)
        self.assertEqual(25, len(sections))
        self.assertEqual(42, sections[23].palette[0])
        self.assertEqual(7, sections[24].palette[0])


class SelectiveNBT(unittest.TestCase):
    def test_unfinished_current_chunk_is_rejected(self):
        r = Region(0, 15, 0, 15)
        prefix = chunk_prefix(0, 0)
        old = DB({prefix + b"\x2b": b"biomes", prefix + b"\x36": struct.pack("<i", 2)})
        current = DB(
            {prefix + b"\x2b": b"biomes", prefix + b"\x36": struct.pack("<i", 1)}
        )
        with self.assertRaisesRegex(ValueError, "Fully generated"):
            plan_chunks(old, current, r)

    def test_nested_map_reference_keeps_current_shared_data_or_refuses_missing(self):
        item = n.CompoundTag({"tag": n.CompoundTag({"map_uuid": n.LongTag(5)})})
        with self.assertRaises(ValueError):
            shared.check(item, DB({b"map_5": b"old"}), DB())
        shared.check(item, DB({b"map_5": b"old"}), DB({b"map_5": b"current"}))

    def test_container_outside_raw_and_nested_items_preserved(self):
        r = Region(1, 2, 1, 2)
        inside = entity(
            1,
            65,
            1,
            Items=n.ListTag(
                [
                    n.CompoundTag(
                        {
                            "Name": n.StringTag("minecraft:shulker_box"),
                            "tag": n.CompoundTag({"nested": n.LongTag(2**40)}),
                        }
                    )
                ],
                10,
            ),
        )
        outside = entity(0, 65, 1, Items=n.ListTag([], 10))
        result = merge_block_entities(inside, outside + entity(1, 65, 1), r)
        self.assertEqual(outside + inside, result)

    def test_cross_boundary_double_chest_may_not_change_partner(self):
        pair = {"pairx": n.IntTag(0), "pairz": n.IntTag(1), "pairlead": n.ByteTag(1)}
        old = entity(1, 65, 1, **pair)
        self.assertEqual(old, merge_block_entities(old, old, Region(1, 2, 1, 2)))
        with self.assertRaisesRegex(ValueError, "pair"):
            merge_block_entities(old, None, Region(1, 2, 1, 2))

    def test_ticks_keep_outside_delays_and_rebase_restored_delays(self):
        def tick(x, time):
            return n.CompoundTag(
                {
                    "x": n.IntTag(x),
                    "y": n.IntTag(65),
                    "z": n.IntTag(1),
                    "time": n.LongTag(time),
                }
            )

        def scheduled(t, ticks):
            return nbt.dump(
                n.CompoundTag(
                    {"currentTick": n.IntTag(t), "tickList": n.ListTag(ticks, 10)}
                )
            )

        old = scheduled(100, [tick(1, 107), tick(2, -1), tick(0, 109)])
        current = scheduled(900, [tick(0, 911), tick(1, 915)])
        result = nbt.compound(merge_ticks(old, current, Region(1, 2, 1, 2)))
        self.assertEqual([911, 907, -1], [int(t["time"]) for t in result["tickList"]])
        self.assertEqual(900, int(result["currentTick"]))

    def test_inventories_preserve_position_xp_permissions_and_sync_hand(self):
        xuid = "123456789"
        ident = str(
            uuid.UUID(
                bytes=hashlib.md5(("pocket-auth-1-xuid:" + xuid).encode()).digest(),
                version=3,
            )
        )
        key = b"player_server_test"
        binding = nbt.dump(
            n.CompoundTag(
                {"MsaId": n.StringTag(ident), "ServerId": n.StringTag(key.decode())}
            )
        )

        def player(label, xp, selected):
            data = n.CompoundTag(
                {
                    "Pos": n.ListTag([n.FloatTag(xp)] * 3, 5),
                    "PlayerLevel": n.IntTag(xp),
                    "SelectedInventorySlot": n.IntTag(selected),
                    "Mainhand": n.ListTag([], 10),
                    "abilities": n.CompoundTag({"op": n.ByteTag(xp % 2)}),
                }
            )
            data["Inventory"] = n.ListTag(
                [
                    n.CompoundTag(
                        {
                            "Name": n.StringTag(label),
                            "Count": n.ByteTag(1),
                            "Slot": n.ByteTag(i),
                        }
                    )
                    for i in range(36)
                ],
                10,
            )
            for field in ["Armor", "Offhand", "EnderChestInventory"]:
                data[field] = n.ListTag(
                    [n.CompoundTag({"Name": n.StringTag(label)})], 10
                )
            return data

        left, right = player("old", 3, 2), player("current", 20, 7)
        source = DB({key: nbt.dump(left), ("player_" + ident).encode(): binding})
        current = DB({key: nbt.dump(right), ("player_" + ident).encode(): binding})
        patch, _ = plan_players(
            source, current, [{"name": "test", "xuid": xuid, "key": key.decode()}]
        )
        result = nbt.compound(patch[key])
        for f in ["Pos", "PlayerLevel", "abilities", "SelectedInventorySlot"]:
            self.assertEqual(right[f], result[f])
        self.assertEqual(left["Inventory"], result["Inventory"])
        self.assertEqual("old", str(result["Mainhand"][0]["Name"]))
        self.assertNotIn("Slot", result["Mainhand"][0])
        with self.assertRaises(ValueError):
            plan_players(
                source, current, [{"name": "wrong", "xuid": "5", "key": key.decode()}]
            )


class ActorBoundaries(unittest.TestCase):
    def test_changed_pet_owner_identity_is_rejected(self):
        old, current = DB(), DB()
        key = actor(old, 1, 1, 1)
        pet = nbt.compound(old[key])
        pet["OwnerNew"] = n.LongTag(100)
        old[key] = nbt.dump(pet)
        old[b"player_server_owner"] = nbt.dump(
            n.CompoundTag({"UniqueID": n.LongTag(100)})
        )
        current[b"player_server_owner"] = nbt.dump(
            n.CompoundTag({"UniqueID": n.LongTag(200)})
        )
        with self.assertRaisesRegex(ValueError, "ownership"):
            plan_actors(old, current, Region(0, 15, 0, 15), self.excluded)

    excluded = {"minecraft:player", "rvo:flag"}

    def test_flags_outside_and_other_dimensions_are_preserved(self):
        old, current = DB(), DB()
        old_key = actor(old, 1, 1, 1)
        new_key = actor(current, 2, 2, 2)
        flag = actor(current, 3, 3, 3, "rvo:flag")
        actor(old, 4, 4, 4, "rvo:flag")
        outside = actor(current, 5, 20, 20)
        nether = actor(current, 6, 2, 2, dimension=1)
        patch, report = plan_actors(old, current, Region(0, 15, 0, 15), self.excluded)
        merged = DB(current)
        for k, v in patch.items():
            if v is None:
                merged.pop(k, None)
            else:
                merged[k] = v
        self.assertNotIn(new_key, merged)
        self.assertEqual(old[old_key], merged[old_key])
        for key in [flag, outside, nether]:
            self.assertEqual(current[key], merged[key])
        registry(merged, Region(0, 15, 0, 15), self.excluded)
        self.assertEqual(1, report["restored"])

    def test_same_individual_now_outside_is_not_duplicated_or_moved(self):
        old, current = DB(), DB()
        key = actor(old, 1, 1, 1)
        actor(current, 1, 25, 25)
        patch, report = plan_actors(old, current, Region(0, 15, 0, 15), self.excluded)
        self.assertNotIn(key, patch)
        self.assertEqual(1, len(report["outside_individuals_preserved"]))

    def test_cross_boundary_passenger_refuses_destructive_edit(self):
        old, current = DB(), DB()
        actor(old, 1, 1, 1, links=(-2,))
        actor(old, 2, 20, 20)
        with self.assertRaisesRegex(ValueError, "outside"):
            plan_actors(old, current, Region(0, 15, 0, 15), self.excluded)

    def test_orphan_records_not_resurrected(self):
        old, current = DB(), DB()
        key = actor(old, 1, 1, 1)
        del old[b"digp" + chunk_prefix(0, 0)]
        patch, _ = plan_actors(old, current, Region(0, 15, 0, 15), self.excluded)
        self.assertNotIn(key, patch)

    def test_existing_stale_references_outside_are_retained(self):
        old, current = DB(), DB()
        key = actor(current, 10, 50, 50)
        stale = b"digp" + chunk_prefix(20, 20)
        current[stale] = key[len(b"actorprefix") :] + b"\xff" * 8
        actor(old, 1, 1, 1)
        patch, _ = plan_actors(old, current, Region(0, 15, 0, 15), self.excluded)
        self.assertNotIn(stale, patch)
        self.assertNotIn(key, patch)

    def test_ambiguous_references_inside_are_rejected(self):
        old, current = DB(), DB()
        key = actor(old, 1, 1, 1)
        old[b"digp" + chunk_prefix(1, 1)] = key[len(b"actorprefix") :]
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            plan_actors(old, current, Region(0, 15, 0, 15), self.excluded)


class NativeOfflineBuild(unittest.TestCase):
    def test_snapshot_to_candidate_and_unchanged_originals(self):
        import leveldb

        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            players = [
                {"name": "A", "key": "player_server_A", "xuid": "123456789"},
                {"name": "B", "key": "player_server_B", "xuid": "987654321"},
            ]
            original_hashes = {}
            for label in ["source", "baseline"]:
                snapshot = root / label
                data = snapshot / "data/worlds/Test/db"
                data.mkdir(parents=True)
                db = leveldb.LevelDB(str(data), True)
                prefix = chunk_prefix(0, 0)
                layer = palettes.Storage(np.zeros(4096, np.uint16), [block(label)])
                db[prefix + b"\x2f\x04"] = b"\x09\x01\x04" + palettes.pack(layer)
                db[prefix + b"\x2b"] = (
                    bytes(512)
                    + b"\x01"
                    + struct.pack("<I", 1 if label == "source" else 2)
                    + b"\xff" * 24
                )
                db[prefix + b"\x2c"] = b"\x2a"
                db[prefix + b"\x36"] = struct.pack("<i", 2)
                db[prefix + b"\x3f"] = label.encode()
                db[prefix + b"\x77"] = b"current-structure-" + label.encode()
                db[b"DynamicProperties"] = b"opaque-current-team-data"
                db[b"other-data"] = b"opaque-" + label.encode()
                for player in players:
                    ident = str(
                        uuid.UUID(
                            bytes=hashlib.md5(
                                ("pocket-auth-1-xuid:" + player["xuid"]).encode()
                            ).digest(),
                            version=3,
                        )
                    )
                    db[("player_" + ident).encode()] = nbt.dump(
                        n.CompoundTag(
                            {
                                "MsaId": n.StringTag(ident),
                                "ServerId": n.StringTag(player["key"]),
                            }
                        )
                    )
                    record = n.CompoundTag(
                        {
                            "Pos": n.ListTag(
                                [n.FloatTag(100 if label == "source" else 200)] * 3, 5
                            ),
                            "SelectedInventorySlot": n.IntTag(7),
                            "PlayerLevel": n.IntTag(9 if label == "source" else 99),
                            "Mainhand": n.ListTag([], 10),
                        }
                    )
                    record["Inventory"] = n.ListTag(
                        [
                            n.CompoundTag(
                                {
                                    "Slot": n.ByteTag(i),
                                    "Name": n.StringTag(label),
                                    "Count": n.ByteTag(i + 1),
                                }
                            )
                            for i in range(36)
                        ],
                        10,
                    )
                    for name in ["Armor", "Offhand", "EnderChestInventory"]:
                        record[name] = n.ListTag(
                            [n.CompoundTag({"Name": n.StringTag(label)})], 10
                        )
                    db[player["key"].encode()] = nbt.dump(record)
                db.close()
                files = {
                    str(p.relative_to(snapshot / "data")): {
                        "bytes": p.stat().st_size,
                        "sha256": hashlib.sha256(p.read_bytes()).hexdigest(),
                    }
                    for p in (snapshot / "data").rglob("*")
                    if p.is_file()
                }
                metadata = {"schema": 1, "id": label, "files": files}
                (snapshot / "snapshot.json").write_text(json.dumps(metadata))
                original_hashes[label] = files
            workspace = root / "workspace"
            seed(workspace, "source", root / "source")
            seed(workspace, "baseline", root / "baseline")
            plan = {
                "world": "Test",
                "dimension": "minecraft:overworld",
                "region": {"x_min": 1, "x_max": 2, "z_min": 1, "z_max": 2},
                "players": players,
                "excluded_entities": ["minecraft:player", "rvo:flag"],
                "missing_source_chunks": [],
            }
            planfile = root / "plan.json"
            planfile.write_text(json.dumps(plan))
            report = build(workspace, planfile)
            self.assertEqual(1, report["chunks"]["boundary_chunks"])
            db = leveldb.LevelDB(str(workspace / "candidate-copy/worlds/Test/db"))
            try:
                cells = palettes.subchunk(db[prefix + b"\x2f\x04"], 4)[0]
                at = lambda x, z: str(
                    nbt.compound(cells.palette[int(cells.indices[x * 256 + z * 16])])[
                        "name"
                    ]
                )
                self.assertEqual("source", at(1, 1))
                self.assertEqual("baseline", at(0, 1))
                self.assertEqual(b"baseline", db[prefix + b"\x3f"])
                self.assertEqual(b"current-structure-baseline", db[prefix + b"\x77"])
                self.assertEqual(b"opaque-baseline", db[b"other-data"])
                for player in players:
                    value = nbt.compound(db[player["key"].encode()])
                    self.assertEqual(200, float(value["Pos"][0]))
                    self.assertEqual(99, int(value["PlayerLevel"]))
                    self.assertEqual("source", str(value["Inventory"][0]["Name"]))
                    self.assertEqual(8, int(value["Mainhand"][0]["Count"]))
            finally:
                db.close()
            for label, files in original_hashes.items():
                for name, entry in files.items():
                    self.assertEqual(
                        entry["sha256"],
                        hashlib.sha256(
                            (root / label / "data" / name).read_bytes()
                        ).hexdigest(),
                    )
            with self.assertRaisesRegex(ValueError, "already exists"):
                build(workspace, planfile)


if __name__ == "__main__":
    unittest.main()
