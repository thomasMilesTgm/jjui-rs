#!/usr/bin/env python3
"""Render AUR recipes from a published crate and verified release archive bytes."""
import argparse
import hashlib
from pathlib import Path
import re
import subprocess
import tarfile
import tomllib
import urllib.request

TEMPLATES = Path(__file__).resolve().parents[1] / 'packaging' / 'aur'
TARGETS = {'X86_64': 'x86_64-unknown-linux-gnu', 'AARCH64': 'aarch64-unknown-linux-gnu'}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--assets', type=Path, default=Path('dist'))
    parser.add_argument('--crate', type=Path, help='Use an already downloaded crates.io archive')
    parser.add_argument('--output', type=Path, default=Path('target/aur'))
    parser.add_argument('--source-only', action='store_true', help='Generate the source and Git recipes before binaries are available')
    args = parser.parse_args()
    if not re.fullmatch(r'\d+\.\d+\.\d+', args.version):
        parser.error('version must be a stable major.minor.patch release')
    args.output.mkdir(parents=True, exist_ok=True)
    crate = args.crate
    if crate is None:
        crate = args.output / f'jjui-rs-{args.version}.crate'
        urllib.request.urlretrieve(f'https://static.crates.io/crates/jjui-rs/jjui-rs-{args.version}.crate', crate)
    with tarfile.open(crate) as archive:
        manifest = tomllib.loads(archive.extractfile(f'jjui-rs-{args.version}/Cargo.toml').read().decode())
    package = manifest['package']
    if package['name'] != 'jjui-rs' or package['version'] != args.version:
        parser.error('crate metadata does not match the requested release')
    major, minor, _ = map(int, args.version.split('.'))
    if manifest['dependencies']['jj-lib']['version'] != f'{major}.{minor}':
        parser.error('jj-lib requirement must match the app major/minor version')
    values = {'VERSION': args.version, 'RUST_VERSION': package['rust-version'],
              'JJ_MIN': f'{major}.{minor}', 'JJ_MAX': f'{major}.{minor + 1}',
              'CRATE_SHA256': digest(crate)}
    if not args.source_only:
        for arch, target in TARGETS.items():
            name = f'jjui-rs-{args.version}-{target}'
            path = args.assets / f'{name}.tar.gz'
            # Check archive layout before publishing a recipe that cannot install it.
            with tarfile.open(path) as archive:
                for member in ['jjui-rs', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE']:
                    if not archive.getmember(f'{name}/{member}').isfile():
                        parser.error(f'{path}: missing regular file {member}')
            values[f'SHA256_{arch}'] = digest(path)
    for template in sorted(TEMPLATES.glob('*.PKGBUILD.in')):
        name = template.name.removesuffix('.PKGBUILD.in')
        if args.source_only and name == 'jjui-rs-bin':
            continue
        text = template.read_text()
        for key, value in values.items():
            text = text.replace(f'@{key}@', value)
        if re.search(r'@[A-Z0-9_]+@', text):
            parser.error(f'unresolved template values in {template}')
        directory = args.output / name
        directory.mkdir(exist_ok=True)
        (directory / 'PKGBUILD').write_text(text)
        srcinfo = subprocess.check_output(['makepkg', '--printsrcinfo'], cwd=directory, text=True)
        (directory / '.SRCINFO').write_text(srcinfo)
        print(directory)


if __name__ == '__main__':
    main()
