#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Build the Headroom e2e's actual subjects before nextest's per-test deadline."""
import json
import os
from pathlib import Path
import subprocess


def build_commands(metadata, root):
    owners = [(package, target) for package in metadata["packages"]
              for target in package["targets"]
              if target["name"] == "busbar-plugin-pack" and "bin" in target["kind"]]
    if len(owners) != 1:
        raise ValueError("the admitted checkout must declare exactly one plugin-packer owner")
    package, target = owners[0]
    pack = ["cargo", "+1.98.0", "build", "--locked", "--manifest-path", str(root / "Cargo.toml"),
            "-p", package["name"], "--bin", "busbar-plugin-pack"]
    features = target.get("required-features", [])
    if features:
        pack += ["--features", ",".join(features)]
    return [["cargo", "+1.98.0", "build", "--locked", "--manifest-path",
             str(root / "dist/default/Cargo.toml"), "--bin", "busbar"], pack]


def main():
    root = Path(os.environ["BUSBAR_CHECKOUT"]).resolve(strict=True)
    target = Path(os.environ["CARGO_TARGET_DIR"])
    if not target.is_absolute() or not target.is_dir():
        raise ValueError("CARGO_TARGET_DIR must be the existing absolute assigned target")
    metadata = subprocess.run(["cargo", "+1.98.0", "metadata", "--locked", "--no-deps",
                               "--format-version", "1", "--manifest-path", str(root / "Cargo.toml")],
                              cwd=root, check=True, capture_output=True, text=True)
    # Both the controller's preparation hook and the e2e invoke these locked builds. Cargo checks
    # the exact admitted source again before reuse; mere existence of an executable never suffices.
    for command in build_commands(json.loads(metadata.stdout), root):
        subprocess.run(command, cwd=root, check=True)
    for name in ("busbar", "busbar-plugin-pack"):
        if not (target / "debug" / name).is_file():
            raise ValueError(f"the actual {name} binary was not produced in the assigned target")


if __name__ == "__main__":
    main()
