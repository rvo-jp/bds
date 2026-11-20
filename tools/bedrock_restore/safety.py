"""Capture a migration-only safety copy after an explicitly authorized clean stop.

This is outside bdsctl's retention-managed snapshots: ordinary `backup` is
deliberately rejected during manual maintenance. No stop/start is performed here.
"""

import argparse
import datetime
import fcntl
import json
import os
import shutil
import subprocess
import tarfile
from pathlib import Path

from .filesystem import file_hash, sync_tree
from .model import require_regular_tree


def validate_stop(mode, status, boot, services):
    if mode.get("phase") != "ready":
        raise ValueError("Ready manual-maintenance guard required")
    if (
        status.get("stopped") is not True
        or status.get("failed") is not False
        or type(status.get("child_exit")) is not int
        or status.get("child_exit") != 0
        or status.get("boot") != boot
        or not isinstance(status.get("invocation"), str)
        or not status.get("invocation")
    ):
        raise ValueError("Clean save/exit from this boot required")
    for name, state, pid in services:
        if state not in ("inactive", "failed") or pid != "0":
            raise ValueError(f"Service/timer is not stopped: {name}")
        if name == "bds.service" and state != "inactive":
            raise ValueError(
                "An abnormal game exit cannot be captured as a safety copy"
            )


def capture():
    if os.geteuid() != 0 or os.uname().sysname != "Linux":
        raise ValueError("Root on Linux required")
    import tomllib

    config = tomllib.loads(Path("/etc/bds/config.toml").read_text())
    paths = config.get("paths", {})
    if (
        paths.get("store", "/var/lib/bds") != "/var/lib/bds"
        or paths.get("runtime", "/run/bds") != "/run/bds"
        or paths.get("releases", "/opt/bedrock/releases") != "/opt/bedrock/releases"
        or config.get("server", {}).get("unit", "bds.service") != "bds.service"
    ):
        raise ValueError(
            "This safety procedure supports the documented default production layout only"
        )
    store = Path("/var/lib/bds")
    # Share the manager's operation lock. Existing file only: never create a lock in a wrong tree.
    with (store / "maintenance.lock").open("r+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        for name in ("transaction.json", "import.json", "reboot-pending.json"):
            if (store / "state" / name).exists():
                raise ValueError(
                    "Unresolved manager transaction requires recovery first"
                )
        mode = json.loads((store / "state/maintenance-mode.json").read_text())
        status = json.loads(Path("/run/bds/status.json").read_text())
        services = []
        units = [
            "bds.service",
            "bds-discord.service",
            "bds-daily.timer",
            "bds-update.timer",
            "bds-daily.service",
            "bds-update.service",
            "bds-discord-restart.service",
            "bds-discord-reboot.service",
        ]
        for unit in units:

            def prop(name):
                return subprocess.check_output(
                    ["systemctl", "show", unit, "-p", name, "--value"], text=True
                ).strip()

            pid = prop("MainPID")
            if unit.endswith(".timer") and not pid:
                pid = "0"
            services.append((unit, prop("ActiveState"), pid))
            group = prop("ControlGroup")
            if group:
                processes = Path("/sys/fs/cgroup" + group) / "cgroup.procs"
                if processes.exists() and processes.read_text().strip():
                    raise ValueError("A stopped service still has child processes")
        validate_stop(
            mode,
            status,
            Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
            services,
        )
        require_regular_tree(store / "data")
        ident = "bds-migration-safety-" + datetime.datetime.now(
            datetime.timezone.utc
        ).strftime("%Y%m%dT%H%M%SZ")
        destination = Path("/var/backups") / ident
        destination.mkdir(mode=0o700)
        shutil.copytree(store / "data", destination / "data")
        files = {}
        for path in (destination / "data").rglob("*"):
            if not path.is_file():
                continue
            relative = str(path.relative_to(destination / "data"))
            digest = file_hash(path)
            if digest != file_hash(store / "data" / relative):
                raise ValueError("Stopped data changed during safety capture")
            files[relative] = {"bytes": path.stat().st_size, "sha256": digest}
        # Save exactly the installed manager/units too; the ordinary world backup excludes them.
        manager_files = [
            Path("/usr/local/bin/bdsctl"),
            Path("/usr/local/share/bdsctl/BUILD.json"),
            Path("/etc/bds/config.toml"),
            Path("/etc/polkit-1/rules.d/50-bds-discord.rules"),
        ]
        manager_files += list(Path("/etc/systemd/system").glob("bds*.service"))
        manager_files += list(Path("/etc/systemd/system").glob("bds*.timer"))
        archive = destination / "manager.tar"
        with tarfile.open(archive, "w") as tar:
            for path in manager_files:
                if path.exists() or path.is_symlink():
                    tar.add(path, arcname=str(path.relative_to("/")), recursive=False)
        archive.chmod(0o600)
        sync_tree(destination)
        release = Path("/opt/bedrock/releases/current").resolve().name
        manifest = {
            "schema": 1,
            "id": ident,
            "release": release,
            "reason": "migration-safety",
            "created": int(datetime.datetime.now(datetime.timezone.utc).timestamp()),
            "files": files,
            "manager_sha256": file_hash(archive),
            "clean_stop": status,
        }
        target = destination / "snapshot.json"
        with target.open("x") as handle:
            json.dump(manifest, handle, indent=2)
            handle.flush()
            os.fsync(handle.fileno())
        target.chmod(0o600)
        descriptor = os.open(destination, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
        print(destination)
        return destination


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--capture-after-authorized-stop", action="store_true", required=True
    )
    parser.parse_args()
    capture()
