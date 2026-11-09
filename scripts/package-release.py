#!/usr/bin/env python3
"""Bundle a native Linux release binary and its documentation for jjui-rs-bin."""
import argparse
from pathlib import Path
import subprocess
import tarfile
import tempfile
import shutil
import tomllib


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=Path('.'))
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--target', choices=['x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu'], required=True)
    parser.add_argument('--output', type=Path, default=Path('dist'))
    args = parser.parse_args()
    version = tomllib.loads((args.source / 'Cargo.toml').read_text())['package']['version']
    with args.binary.open('rb') as binary:
        header = binary.read(20)
    machine = 62 if args.target.startswith('x86_64-') else 183
    if (header[:6] != b'\x7fELF\x02\x01' or len(header) < 20
            or int.from_bytes(header[18:20], 'little') != machine):
        parser.error(f'binary is not a 64-bit Linux ELF for {args.target}')
    actual = subprocess.check_output([str(args.binary.resolve()), '--version'], text=True).strip()
    if actual != f'jjui-rs {version}':
        parser.error(f'binary version {actual!r} does not match {version}')
    args.output.mkdir(parents=True, exist_ok=True)
    name = f'jjui-rs-{version}-{args.target}'
    with tempfile.TemporaryDirectory() as tmp:
        stage = Path(tmp) / name
        stage.mkdir()
        shutil.copy2(args.binary, stage / 'jjui-rs')
        for path in ['README.md', 'LICENSE-MIT', 'LICENSE-APACHE']:
            shutil.copy2(args.source / path, stage / path)
        for path in ['docs', 'keybinds', 'themes', 'examples']:
            shutil.copytree(args.source / path, stage / path)
        with tarfile.open(args.output / f'{name}.tar.gz', 'w:gz') as archive:
            archive.add(stage, arcname=name)
    print(args.output / f'{name}.tar.gz')


if __name__ == '__main__':
    main()
