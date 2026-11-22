"""Archive the executable and license notices for a release."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tomllib
import zipfile


def licenses():
    rust = subprocess.check_output(["rustc", "-vV"], text=True)
    target = next(line.removeprefix("host: ") for line in rust.splitlines() if line.startswith("host: "))
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target],
    ))
    for package in metadata["packages"]:
        name = package["name"]
        if name == "auto-paint":
            continue
        version = package["version"]
        root = Path(package["manifest_path"]).parent
        if package["license_file"]:
            notice = root / package["license_file"]
            yield notice, f"licenses/{name}-{version}/{notice.name}"
        for notice in sorted(root.iterdir()):
            if notice.is_file() and notice.name.upper().startswith(("LICENSE", "COPYING", "NOTICE", "UNLICENSE")):
                if notice.name != package["license_file"]:
                    yield notice, f"licenses/{name}-{version}/{notice.name}"
        if name == "paintrs":
            notice = root / "assets/Liberation-LICENSE.txt"
            yield notice, f"licenses/{name}-{version}/{notice.name}"
        if name == "openh264-sys2":
            notice = root / "upstream/LICENSE"
            yield notice, f"licenses/{name}-{version}/OpenH264-LICENSE"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=("linux-x86_64", "windows-x86_64", "macos-aarch64", "macos-x86_64"))
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    windows = args.platform.startswith("windows-")
    version = tomllib.loads(Path("Cargo.toml").read_text())["package"]["version"]
    stem = f"auto-paint-{version}-{args.platform}"
    output = Path("target/dist")
    output.mkdir(parents=True, exist_ok=True)
    files = [
        (args.binary, "auto-paint.exe" if windows else "auto-paint"),
        (Path("README.md"), "README.md"),
        (Path("LICENSE"), "LICENSE"),
        (Path("assets/Liberation-LICENSE.txt"), "licenses/Liberation-LICENSE.txt"),
        *licenses(),
    ]
    if windows:
        archive = output / f"{stem}.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, strict_timestamps=False) as bundle:
            for path, name in files:
                bundle.write(path, name)
    else:
        archive = output / f"{stem}.tar.gz"
        with tarfile.open(archive, "w:gz") as bundle:
            for path, name in files:
                bundle.add(path, arcname=name)
    with archive.open("rb") as data:
        checksum = hashlib.file_digest(data, "sha256").hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(
        f"{checksum}  {archive.name}\n", encoding="ascii", newline="\n"
    )
    print(archive)


if __name__ == "__main__":
    main()
