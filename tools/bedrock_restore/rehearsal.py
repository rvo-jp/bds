"""Run the staged manager, official BDS and updated packs on a disposable copy.

Root/Linux only. The test unit has its own network and cannot access production
data/config/state. It never installs manager binaries, packs or production units.
"""

import argparse
import copy
import json
import os
import pwd
import shutil
import subprocess
import tarfile
import time
import uuid
from pathlib import Path

from . import nbt
from .database import opened
from .filesystem import workspace_lock
from .model import guarded_root, read_plan


def run(*args, timeout=240, check=True):
    result = subprocess.run(
        [str(v) for v in args], capture_output=True, text=True, timeout=timeout
    )
    if check and result.returncode:
        raise RuntimeError(
            f"{args[0]} failed ({result.returncode}): {result.stdout}\n{result.stderr}"
        )
    return result


def signatures(db, plan, flag_keys):
    flags = {}
    for key in flag_keys:
        data = nbt.compound(db.get(bytes.fromhex(key)))
        if str(data["identifier"]) != "rvo:flag":
            raise ValueError("Expected current team flag")
        health = [
            copy.deepcopy(a)
            for a in data["Attributes"]
            if str(a["Name"]) == "minecraft:health"
        ]
        flags[key] = {
            "position": copy.deepcopy(data["Pos"]),
            "health": health,
            "properties": copy.deepcopy(data.get("DynamicProperties")),
        }
    players = {
        p["key"]: nbt.compound(db.get(p["key"].encode())) for p in plan["players"]
    }
    return flags, players, db.get(b"DynamicProperties")


def rehearse(workspace, manager, expected_commit):
    if os.geteuid() != 0 or os.uname().sysname != "Linux":
        raise ValueError("Root on Linux required")
    root = guarded_root(workspace)
    with workspace_lock(root):
        return rehearse_locked(root, manager, expected_commit)


def rehearse_locked(root, manager, expected_commit):
    plan, region = read_plan(root / "restore-plan.json")
    report = json.loads((root / "restore-report.json").read_text())
    if report["plan"] != plan:
        raise ValueError("Plan differs from offline verification report")
    manager = Path(manager).resolve(strict=True)
    if f"({expected_commit})" not in run(manager, "--version").stdout:
        raise ValueError("Manager does not match pinned commit")
    addon = json.loads((root / "addon-build.json").read_text())
    inspection = json.loads((root / "baseline-inspection.json").read_text())
    flag_keys = [entry["key"] for entry in inspection["actors"]["flags"]]
    release = json.loads((root / "baseline-snapshot.json").read_text())["release"]
    test = Path("/var/lib") / ("bds-prep-" + uuid.uuid4().hex[:12])
    unit = test.name + ".service"
    unitfile = Path("/run/systemd/system") / unit
    account = pwd.getpwnam("bds")
    started = time.monotonic()
    created_unit = False
    result = {
        "manager_commit": expected_commit,
        "addon_commit": addon["commit"],
        "bds_release": release,
        "network": "private",
        "production_paths": "inaccessible",
    }
    test.mkdir(mode=0o750)
    os.chown(test, 0, account.pw_gid)
    try:
        binary = test / "bdsctl"
        shutil.copy2(manager, binary)
        binary.chmod(0o755)
        store = test / "store"
        store.mkdir()
        data = store / "data"
        shutil.copytree(root / "candidate-copy", data)
        # Only this disposable copy is modified by BDS. It is never a deployment candidate.
        for family, folder in [
            ("development_behavior_packs", "rvo-smp-bp"),
            ("development_resource_packs", "rvo-smp-rp"),
        ]:
            shutil.rmtree(data / family / folder)
        with tarfile.open(root / "rvo-smp-addon.tar.gz") as archive:
            if {m.name for m in archive.getmembers()} != set(addon["files"]):
                raise ValueError(
                    "Addon archive membership differs from the verified build"
                )
            for member in archive.getmembers():
                if (
                    not member.isfile()
                    or Path(member.name).is_absolute()
                    or ".." in Path(member.name).parts
                ):
                    raise ValueError("Unsafe addon archive")
                raw = archive.extractfile(member).read()
                import hashlib

                if hashlib.sha256(raw).hexdigest() != addon["files"][member.name]:
                    raise ValueError("Addon checksum mismatch")
                folder, relative = member.name.split("/", 1)
                if folder not in ("rvo-smp-bp", "rvo-smp-rp"):
                    raise ValueError("Unexpected addon pack root")
                family = (
                    "development_behavior_packs"
                    if folder == "rvo-smp-bp"
                    else "development_resource_packs"
                )
                destination = data / family / folder / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(raw)
        world = data / "worlds" / plan["world"]
        for name, pack in [
            ("world_behavior_packs.json", "rvo-smp-bp"),
            ("world_resource_packs.json", "rvo-smp-rp"),
        ]:
            header = addon["packs"][pack]
            entries = json.loads((world / name).read_text())
            entries = [entry for entry in entries if entry["pack_id"] != header["uuid"]]
            entries.append({"pack_id": header["uuid"], "version": header["version"]})
            (world / name).write_text(json.dumps(entries))
        props = {}
        for line in (data / "server.properties").read_text().splitlines():
            if "=" in line and not line.startswith("#"):
                key, value = line.split("=", 1)
                props[key] = value
        props.update(
            {
                "online-mode": "false",
                "allow-list": "false",
                "allow-cheats": "true",
                "enable-lan-visibility": "false",
                "view-distance": "5",
                "tick-distance": "4",
                "max-threads": "1",
            }
        )
        (data / "server.properties").write_text(
            "".join(f"{k}={v}\n" for k, v in props.items())
        )
        shutil.rmtree(data / "keys", ignore_errors=True)
        releases = test / "releases"
        releases.mkdir()
        shutil.copytree(Path("/opt/bedrock/releases") / release, releases / release)
        (releases / "current").symlink_to(release)
        config = test / "config.toml"
        config.write_text(f'''[paths]
store="{store}"
releases="{releases}"
runtime="{test}/runtime"
diagnostics="{test}/diagnostics"
discord_state="{test}/discord"
webhook_file="{test}/no-webhook"
[server]
user="bds"
unit="{unit}"
startup_seconds=180
stable_seconds=2
[maintenance]
notice_seconds=0
minimum_free_mib=0
''')
        config.chmod(0o640)
        os.chown(config, 0, account.pw_gid)

        def ctl(*args, **kwargs):
            return run(binary, "--config", config, *args, **kwargs)

        ctl("prepare-directories")
        run("chown", "-R", "bds:bds", store)
        ctl("generate-units", test / "units", binary)
        text = (test / "units" / unit).read_text()
        text = "\n".join(
            line for line in text.splitlines() if not line.startswith("OnFailure=")
        )
        text += """
[Service]
PrivateNetwork=yes
Restart=no
CPUQuota=50%
MemoryMax=2G
IOWeight=10
Nice=19
TimeoutStartSec=210s
TimeoutStopSec=30s
RuntimeMaxSec=240s
SendSIGKILL=yes
InaccessiblePaths=/var/lib/bds /etc/bds /run/bds /var/lib/bds-discord /var/lib/bds-supervisor
"""
        unitfile.write_text(text + "\n")
        created_unit = True
        run("systemctl", "daemon-reload")
        with opened(world / "db") as db:
            before = signatures(db, plan, flag_keys)
        run("systemctl", "start", unit, timeout=240)
        private = run(
            "systemctl", "show", unit, "-p", "PrivateNetwork", "--value"
        ).stdout.strip()
        pid = run("systemctl", "show", unit, "-p", "MainPID", "--value").stdout.strip()
        if private != "yes" or not pid.isdigit() or int(pid) == 0:
            raise ValueError("Private network/unit process not verified")
        namespace = os.readlink(f"/proc/{pid}/ns/net")
        if namespace == os.readlink("/proc/1/ns/net"):
            raise ValueError("Test accidentally shares the host network")
        result["network_namespace"] = namespace
        status = json.loads((test / "runtime/status.json").read_text())
        if not status.get("ready") or status.get("failed"):
            raise ValueError("Isolated BDS did not become ready")
        ctl("console", "tickingarea remove_all")
        coordinates = [
            ((region.x_min + region.x_max) // 2, (region.z_min + region.z_max) // 2),
            (region.x_min, region.z_min),
            (region.x_min, region.z_max),
            (region.x_max, region.z_min),
            (region.x_max, region.z_max),
        ]
        for i, (x, z) in enumerate(coordinates):
            ctl("console", f"tickingarea add circle {x} 64 {z} 1 prep_{i}")
            time.sleep(2)
        ctl("console", "tickingarea list")
        time.sleep(15)
        run("systemctl", "stop", unit, timeout=180)
        peak = run(
            "systemctl", "show", unit, "-p", "MemoryPeak", "--value"
        ).stdout.strip()
        result["memory_peak_bytes"] = int(peak) if peak.isdigit() else None
        logs = run("journalctl", "-u", unit, "--no-pager", "-o", "cat").stdout
        (root / "isolated-bds.log").write_text(logs)
        if "Server started." not in logs:
            raise ValueError("Server-started evidence absent")
        if any(
            token in logs
            for token in (
                "[Scripting][error]",
                "[Scripting][Error]",
                "Failed to load",
                "Script runtime",
                "Syntax error",
                "Syntax Error",
            )
        ):
            raise ValueError("Isolated load/command error; inspect log")
        for i in range(len(coordinates)):
            if f"prep_{i}" not in logs:
                raise ValueError("Region loading evidence absent")
        with opened(world / "db") as db:
            after = signatures(db, plan, flag_keys)
        if before != after:
            raise ValueError(
                "Flags, team data or restored player data changed unexpectedly during rehearsal"
            )
        status = json.loads((test / "runtime/status.json").read_text())
        if not status.get("stopped"):
            raise ValueError("Isolated BDS did not stop normally")
        result.update(
            {
                "status": "passed",
                "regions_loaded": coordinates,
                "flags_preserved": len(flag_keys),
                "players_preserved": len(plan["players"]),
                "seconds": round(time.monotonic() - started, 2),
            }
        )
        (root / "rehearsal-report.json").write_text(json.dumps(result, indent=2))
        print(json.dumps(result), flush=True)
        return result
    finally:
        if created_unit:
            run("systemctl", "stop", unit, check=False)
            log = run(
                "journalctl", "-u", unit, "--no-pager", "-o", "cat", check=False
            ).stdout
            (root / "isolated-bds.log").write_text(log)
            unitfile.unlink(missing_ok=True)
            run("systemctl", "daemon-reload", check=False)
            run("systemctl", "reset-failed", unit, check=False)
        shutil.rmtree(test)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workspace")
    parser.add_argument("manager")
    parser.add_argument("commit")
    args = parser.parse_args()
    rehearse(args.workspace, args.manager, args.commit)
