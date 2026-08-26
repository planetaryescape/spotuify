#!/usr/bin/env python3
"""Build the downloadable spotuify skill and optionally sync the local copy."""

from __future__ import annotations

import argparse
import os
import shutil
import tempfile
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
SOURCE_DIR = REPOSITORY_ROOT / "skills" / "spotuify"
BUNDLE_PATH = REPOSITORY_ROOT / "site" / "public" / "spotuify.skill"
SKILL_FILES = ("SKILL.md", "LICENSE.txt")
ZIP_TIMESTAMP = (1980, 1, 1, 0, 0, 0)


def build_bundle(path: Path) -> None:
    with ZipFile(path, "w", compression=ZIP_DEFLATED, compresslevel=9) as archive:
        for filename in SKILL_FILES:
            info = ZipInfo(filename, date_time=ZIP_TIMESTAMP)
            info.compress_type = ZIP_DEFLATED
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            archive.writestr(info, (SOURCE_DIR / filename).read_bytes())


def sync_local(destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    for filename in SKILL_FILES:
        target = destination / filename
        shutil.copyfile(SOURCE_DIR / filename, target)
        target.chmod(0o644)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail when the committed bundle differs from the skill source",
    )
    parser.add_argument(
        "--sync-local",
        action="store_true",
        help="copy the skill source into BK's local skill directory",
    )
    parser.add_argument(
        "--local-dir",
        type=Path,
        default=Path.home() / ".dotfiles" / ".skills" / "spotuify",
        help="override the local skill directory",
    )
    args = parser.parse_args()

    file_descriptor, temporary_name = tempfile.mkstemp(
        suffix=".skill", dir=BUNDLE_PATH.parent
    )
    os.close(file_descriptor)
    temporary_path = Path(temporary_name)
    try:
        build_bundle(temporary_path)
        if args.check:
            if not BUNDLE_PATH.exists() or temporary_path.read_bytes() != BUNDLE_PATH.read_bytes():
                print("spotuify.skill is stale; run scripts/package_spotuify_skill.py")
                return 1
        else:
            os.replace(temporary_path, BUNDLE_PATH)
            BUNDLE_PATH.chmod(0o644)

        if args.sync_local:
            sync_local(args.local_dir.expanduser())
    finally:
        temporary_path.unlink(missing_ok=True)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
