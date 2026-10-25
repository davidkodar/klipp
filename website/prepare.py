#!/usr/bin/env python3
"""Assemble the static homepage and its verified release attachment; never deploy."""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="New output directory")
    args = parser.parse_args()
    output = args.output.expanduser().absolute()
    if output.exists() or output.is_symlink():
        parser.error("Output already exists; choose a new directory.")
    if not output.parent.is_dir():
        parser.error("Output parent directory must already exist.")
    source = Path(__file__).resolve().parent
    media = json.loads((source / "demo.json").read_text())
    if Path(media["asset"]).name != media["asset"]:
        parser.error("The media asset must be a plain filename.")

    # Stage beside the destination: failed downloads never leave a usable-looking site.
    with tempfile.TemporaryDirectory(prefix=".klipp-site-", dir=output.parent) as temporary:
        stage = Path(temporary) / "site"
        assets = stage / "assets"
        assets.mkdir(parents=True)
        for name in ("index.html", "style.css", "credits.html"):
            shutil.copyfile(source / name, stage / name)
        for name in ("editing.webp", "colour.webp", "klipp-icon.svg", "klipp-wordmark.svg", "klipp-showcase-v3-1080p.webp"):
            shutil.copyfile(source / "assets" / name, assets / name)
        video = assets / media["asset"]
        # Address the pinned asset directly; no draft-release listing is needed.
        with video.open("wb") as stream:
            subprocess.run(
                ["gh", "api", f"repos/{media['repository']}/releases/assets/{media['asset_id']}",
                 "--header", "Accept: application/octet-stream"],
                stdout=stream,
                check=True,
            )
        with video.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        if video.stat().st_size != media["bytes"] or digest != media["sha256"]:
            raise SystemExit("Demo verification failed: release attachment differs from demo.json.")
        (stage / ".nojekyll").touch()
        # Never package credentials, repository files, raw footage, or working notes.
        stage.rename(output)
    print(f"Verified homepage prepared at {output}. Nothing was published.")
    print(f"Preview: python3 -m http.server 8765 --bind 127.0.0.1 --directory {output}")


if __name__ == "__main__":
    main()
