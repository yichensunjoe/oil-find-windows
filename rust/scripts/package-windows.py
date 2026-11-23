"""Verify and replace the local Windows executable."""

import argparse
import errno
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile


def unlink_if_present(path):
    try:
        path.unlink()
    except FileNotFoundError:
        pass


def is_plain_path(path, file_type):
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return False
    reparse = getattr(metadata, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT
    return not reparse and stat.S_IFMT(metadata.st_mode) == file_type


def remove_legacy_outputs(output):
    # Limit cleanup to the files actually shipped by the old packager.
    files = [output / name for name in ("oilfind-cli.exe", "LICENSE", "THIRD_PARTY_NOTICES.txt", "USAGE.txt")]
    licenses = output / "LICENSES"
    has_license_dir = is_plain_path(licenses, stat.S_IFDIR)
    if has_license_dir:
        files.extend(licenses / name for name in ("Apache-2.0.txt", "MIT.txt"))
    for path in files:
        if is_plain_path(path, stat.S_IFREG):
            unlink_if_present(path)
    if has_license_dir:
        try:
            licenses.rmdir()
        except OSError as error:
            if error.errno not in (errno.ENOENT, errno.ENOTEMPTY, errno.EEXIST):
                raise


def package(source, output):
    output.mkdir(parents=True, exist_ok=True)
    verifier = Path(__file__).with_name("verify-pe.py")
    # Stage a sibling file so it inherits the output directory's Windows ACL.
    with tempfile.NamedTemporaryFile(prefix=".oilfind-package-", suffix=".exe", dir=output, delete=False) as staging:
        candidate = Path(staging.name)
    try:
        shutil.copyfile(source, candidate)
        result = subprocess.run(
            [sys.executable, str(verifier), "--arch", "amd64", str(candidate)], check=False
        )
        if result.returncode:
            return result.returncode
        if os.name == "posix":
            shutil.copymode(source, candidate)
        candidate.replace(output / "OilFind.exe")
    finally:
        unlink_if_present(candidate)

    try:
        remove_legacy_outputs(output)
    except OSError as error:
        print(f"Verified OilFind.exe was replaced. Legacy cleanup failed: {error}", file=sys.stderr)
        return 1
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="Built oilfind-win.exe")
    parser.add_argument("output", type=Path, help="Local Windows package directory")
    args = parser.parse_args()
    try:
        return package(args.source, args.output)
    except OSError as error:
        print(f"Windows packaging failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
