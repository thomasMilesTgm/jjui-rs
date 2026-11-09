#!/usr/bin/env python3
"""Offline checks for release archive/PKGBUILD contracts; requires makepkg."""
import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(shutil.which('makepkg'), 'requires Arch makepkg')
class Packaging(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.crate = self.root / 'release.crate'
        self.manifest = b'''[package]
name = "jjui-rs"
version = "0.47.0"
rust-version = "1.97.1"
[dependencies.jj-lib]
version = "0.47"
'''
        self.archive(self.crate, {'jjui-rs-0.47.0/Cargo.toml': self.manifest})
        self.files = {
            'jjui-rs': b'binary fixture, not an executable release',
            'LICENSE-MIT': b'MIT', 'LICENSE-APACHE': b'Apache', 'README.md': b'readme',
            'docs/plugins.md': b'docs', 'keybinds/keybinds.default.toml': b'keys',
            'themes/example.toml': b'theme', 'examples/config.toml': b'config',
        }
        for arch in ['x86_64', 'aarch64']:
            name = f'jjui-rs-0.47.0-{arch}-unknown-linux-gnu'
            self.archive(self.root / f'{name}.tar.gz', {f'{name}/{k}': v for k, v in self.files.items()})

    def archive(self, path, files):
        with tarfile.open(path, 'w:gz') as archive:
            for name, data in files.items():
                entry = tarfile.TarInfo(name)
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))

    def generate(self, *extra):
        return subprocess.run([
            'python3', str(ROOT / 'scripts/prepare-aur.py'), '--version', '0.47.0',
            '--crate', str(self.crate), '--assets', str(self.root),
            '--output', str(self.root / 'recipes'), *extra,
        ], capture_output=True, text=True)

    def test_binary_recipe_installs_both_architectures(self):
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        recipe = self.root / 'recipes/jjui-rs-bin'
        srcinfo = (recipe / '.SRCINFO').read_text()
        self.assertIn('provides = jjui-rs=0.47.0', srcinfo)
        self.assertIn('conflicts = jjui-rs', srcinfo)
        self.assertIn('depends = jujutsu<0.48', srcinfo)
        for arch in ['x86_64', 'aarch64']:
            archive = self.root / f'jjui-rs-0.47.0-{arch}-unknown-linux-gnu.tar.gz'
            self.assertIn(hashlib.sha256(archive.read_bytes()).hexdigest(), srcinfo)
            source = self.root / f'source-{arch}'
            source.mkdir()
            with tarfile.open(archive) as tar:
                tar.extractall(source, filter='data')
            destination = self.root / f'package-{arch}'
            subprocess.run(['bash', '-ec', 'source "$RECIPE"; package'], check=True, env={
                **os.environ, 'RECIPE': str(recipe / 'PKGBUILD'), 'CARCH': arch,
                'srcdir': str(source), 'pkgdir': str(destination),
            })
            self.assertEqual((destination / 'usr/bin/jjui-rs').read_bytes(), self.files['jjui-rs'])
            self.assertTrue((destination / 'usr/share/jjui-rs/examples/config.toml').exists())
            self.assertTrue((destination / 'usr/share/licenses/jjui-rs-bin/LICENSE-MIT').exists())
            self.assertFalse((destination / 'etc').exists())

    def test_missing_binary_fails_instead_of_generating_bad_checksums(self):
        (self.root / 'jjui-rs-0.47.0-aarch64-unknown-linux-gnu.tar.gz').unlink()
        self.assertNotEqual(self.generate().returncode, 0)
        result = self.generate('--source-only')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / 'recipes/jjui-rs-bin').exists())
        srcinfo = (self.root / 'recipes/jjui-rs/.SRCINFO').read_text()
        self.assertIn(hashlib.sha256(self.crate.read_bytes()).hexdigest(), srcinfo)
        self.assertNotIn('SKIP', srcinfo)

    def test_mismatched_release_series_is_rejected(self):
        self.archive(self.crate, {'jjui-rs-0.47.0/Cargo.toml': self.manifest.replace(b'"0.47"', b'"0.48"')})
        result = self.generate('--source-only')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('jj-lib requirement must match', result.stderr)

    def test_archive_builder_rejects_wrong_architecture(self):
        binary = self.root / 'binary'
        binary.write_bytes(b'\x7fELF\x02\x01' + b'\0' * 12 + (62).to_bytes(2, 'little'))
        result = subprocess.run([
            'python3', str(ROOT / 'scripts/package-release.py'), '--source', str(ROOT),
            '--binary', str(binary), '--target', 'aarch64-unknown-linux-gnu',
            '--output', str(self.root / 'out'),
        ], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('not a 64-bit Linux ELF', result.stderr)


if __name__ == '__main__':
    unittest.main()
