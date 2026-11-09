#!/usr/bin/env python3
"""Build a Debian package from an existing native Linux release archive."""
import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

TARGETS = {'amd64': ('x86_64-unknown-linux-gnu', 62), 'arm64': ('aarch64-unknown-linux-gnu', 183)}
MAINTAINER = 'Thomas Miles <thomas.miles.tgm@gmail.com>'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--architecture', choices=TARGETS, required=True)
    parser.add_argument('--archive', type=Path, required=True)
    parser.add_argument('--revision', type=int, default=1)
    parser.add_argument('--output', type=Path, default=Path('dist'))
    args = parser.parse_args()
    if not re.fullmatch(r'\d+\.\d+\.\d+', args.version) or args.revision < 1:
        parser.error('expected a stable release version and positive packaging revision')
    target, machine = TARGETS[args.architecture]
    major, minor, _ = map(int, args.version.split('.'))
    series = f'{major}.{minor}'
    next_series = f'{major}.{minor + 1}'
    args.output.mkdir(parents=True, exist_ok=True)
    output = args.output.resolve() / f'jjui-rs_{args.version}-{args.revision}_{args.architecture}.deb'
    if output.exists():
        parser.error(f'refusing to overwrite {output}')
    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        with tarfile.open(args.archive) as archive:
            archive.extractall(work / 'source', filter='data')
        source = work / 'source' / f'jjui-rs-{args.version}-{target}'
        binary = source / 'jjui-rs'
        header = binary.read_bytes()[:20]
        if (header[:6] != b'\x7fELF\x02\x01' or len(header) < 20
                or int.from_bytes(header[18:20], 'little') != machine):
            parser.error('release binary architecture does not match the package architecture')
        actual = subprocess.check_output([str(binary), '--version'], text=True).strip()
        if actual != f'jjui-rs {args.version}':
            parser.error('release binary version does not match the package version')
        package = work / 'package'
        bindir = package / 'usr/bin'
        bindir.mkdir(parents=True)
        shutil.copy2(binary, bindir / 'jjui-rs')
        subprocess.run(['strip', '--strip-unneeded', str(bindir / 'jjui-rs')], check=True)
        doc = package / 'usr/share/doc/jjui-rs'
        doc.mkdir(parents=True)
        shutil.copy2(source / 'README.md', doc / 'README.md')
        shutil.copytree(source / 'docs', doc / 'docs')
        (doc / 'copyright').write_text(
            'Source: https://github.com/thomasMilesTgm/jjui-rs\n'
            'Licensed under MIT OR Apache-2.0.\n\n'
            + (source / 'LICENSE-MIT').read_text() + '\n'
            + (source / 'LICENSE-APACHE').read_text())
        for directory in ['keybinds', 'themes', 'examples']:
            shutil.copytree(source / directory, package / 'usr/share/jjui-rs' / directory)
        # Resolve shared-library requirements on the matching Ubuntu build runner.
        (work / 'debian').mkdir()
        (work / 'debian/control').write_text(
            f'Source: jjui-rs\nMaintainer: {MAINTAINER}\n\n'
            'Package: jjui-rs\nArchitecture: any\nDescription: Jujutsu terminal UI\n')
        dependencies = subprocess.check_output(
            ['dpkg-shlibdeps', '-O', '-e' + str(bindir / 'jjui-rs')], cwd=work, text=True)
        dependencies = next(line.removeprefix('shlibs:Depends=') for line in dependencies.splitlines()
                            if line.startswith('shlibs:Depends='))
        size = sum(path.stat().st_size for path in package.rglob('*') if path.is_file())
        control = package / 'DEBIAN'
        control.mkdir()
        (control / 'control').write_text(
            f'Package: jjui-rs\nVersion: {args.version}-{args.revision}\n'
            f'Architecture: {args.architecture}\nMaintainer: {MAINTAINER}\n'
            'Section: devel\nPriority: optional\n'
            f'Installed-Size: {(size + 1023) // 1024}\nDepends: {dependencies}\n'
            f'Suggests: jujutsu (>= {series}), jujutsu (<< {next_series})\n'
            'Homepage: https://github.com/thomasMilesTgm/jjui-rs\n'
            'Description: live terminal UI for Jujutsu\n'
            ' Native repository reads keep the graph current without polling jj log.\n'
            f' Install jj {series}.x separately for editing and Git actions.\n')
        subprocess.run(['dpkg-deb', '--root-owner-group', '--build', str(package), str(output)], check=True)
    print(output)


if __name__ == '__main__':
    main()
