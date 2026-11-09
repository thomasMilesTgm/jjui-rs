#!/usr/bin/env python3
"""Download and verify all retained stable-release Debian packages with GitHub CLI."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    repository = os.environ['GH_REPO']
    pages = json.loads(subprocess.check_output(
        ['gh', 'api', '--paginate', '--slurp', f'repos/{repository}/releases'], text=True))
    found = 0
    for release in (release for page in pages for release in page):
        tag = release['tag_name']
        if release['draft'] or release['prerelease'] or not re.fullmatch(r'\d+\.\d+\.\d+', tag):
            continue
        names = [asset['name'] for asset in release['assets']]
        debs = [name for name in names if name.endswith('.deb')]
        if not debs:
            continue
        sums = [name for name in names if name == 'SHA256SUMS' or re.fullmatch(r'DEB-\d+-SHA256SUMS', name)]
        if not sums:
            raise RuntimeError(f'{tag} has packages without a checksum manifest')
        directory = args.output / tag
        directory.mkdir(parents=True, exist_ok=True)
        command = ['gh', 'release', 'download', tag, '--dir', str(directory)]
        for name in [*debs, *sums]:
            if '/' in name or '\\' in name or name in ['.', '..']:
                raise RuntimeError(f'unexpected asset name: {name}')
            command.extend(['--pattern', name])
        subprocess.run(command, check=True)
        expected = {}
        for name in sums:
            for line in (directory / name).read_text().splitlines():
                checksum, filename = line.split(maxsplit=1)
                filename = filename.lstrip('*')
                if filename in expected and expected[filename] != checksum:
                    raise RuntimeError(f'conflicting checksums for {filename}')
                expected[filename] = checksum
        for name in debs:
            if hashlib.sha256((directory / name).read_bytes()).hexdigest() != expected.get(name):
                raise RuntimeError(f'checksum mismatch or missing checksum for {tag}/{name}')
            found += 1
    if not found:
        raise RuntimeError('no published Debian packages found')
    print(f'Verified {found} Debian packages')


if __name__ == '__main__':
    main()
