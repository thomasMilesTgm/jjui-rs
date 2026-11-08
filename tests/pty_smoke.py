"""Unix terminal integration smoke test. Run after cargo build."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import shutil
import struct
import subprocess
import tempfile
import termios
import time

binary = str(Path(__file__).resolve().parents[1] / 'target/debug/jjui-rs')
with tempfile.TemporaryDirectory(prefix='jjui-ui-') as directory:
    root = Path(directory)
    env = {**os.environ, 'JJ_CONFIG': '', 'TERM': 'xterm-256color', 'COLORTERM': 'truecolor'}
    env.pop('NO_COLOR', None)

    def jj(*args):
        subprocess.run(['jj', '--config', 'user.name=Test', '--config', 'user.email=test@example.org', *args], cwd=root, env=env, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

    jj('git', 'init', '--colocate')
    env['XDG_CONFIG_HOME'] = str(root / '.jj/test-config')
    user_config = root / '.jj/test-user.toml'
    user_config.write_text("[revset-aliases]\n'immutable_heads()'='bookmarks(published) | root()'\n")
    env['JJ_CONFIG'] = str(user_config)
    (root / '.jj/repo/config.toml').write_text('[user]\nname="Test"\nemail="test@example.org"\n')
    (root / 'file.txt').write_text('base file\nmiddle\nbase end\n')
    jj('describe', '-m', 'base')
    remote = root / '.jj/test-remote.git'
    subprocess.run(['git', 'init', '--bare', '--quiet', str(remote)], env=env, check=True)
    jj('git', 'remote', 'add', 'origin', str(remote))
    keys = root / '.jj/keybinds.toml'
    keys.write_text('[revisions]\nnew=["x"]\n"plugin.describe"=["z d"]\n"plugin.inspect"=["z i"]\n')
    plugins = root / '.jj/test-plugins'
    shutil.copytree(Path(__file__).resolve().parents[1] / 'examples/plugins', plugins)

    def snapshot():
        result = subprocess.run([binary, '-R', str(root), '--once'], env={**env, 'PATH': ''}, capture_output=True, check=True)
        return json.loads(result.stdout)

    def current():
        return next(r for r in snapshot()['revisions'] if r['working_copy'])

    before_open = current()['commit_id']
    (root / 'startup.txt').write_text('present before opening\n')
    theme = root / '.jj/test-theme.toml'
    theme.write_text('hint_key="#112233"\n')
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 150, 0, 0))
    original = termios.tcgetattr(slave)
    process = subprocess.Popen([binary, '-R', str(root), '--keybinds', str(keys), '--theme', str(theme), '--plugins', str(plugins / 'plugins.toml')], stdin=slave, stdout=slave, stderr=slave, env=env)
    output = bytearray()

    def read(timeout=.1):
        if select.select([master], [], [], timeout)[0]:
            output.extend(os.read(master, 65536))

    def screen():
        rows = [[' '] * 150 for _ in range(40)]
        x = y = 0
        for match in re.finditer(r'\x1b\[([0-9;?]*)([A-Za-z])|([^\x1b])', output.decode('utf-8', errors='replace')):
            params, command, char = match.groups()
            if command:
                if command in ('H', 'f'):
                    numbers = [int(n or '1') for n in params.split(';')]
                    y, x = numbers[0] - 1, (numbers[1] if len(numbers) > 1 else 1) - 1
                elif command == 'J' and params == '2':
                    rows = [[' '] * 150 for _ in range(40)]
            elif char == '\n':
                y, x = y + 1, 0
            elif char == '\r':
                x = 0
            elif char and char.isprintable():
                if 0 <= y < 40 and 0 <= x < 150:
                    rows[y][x] = char
                x += 1
        return '\n'.join(''.join(row) for row in rows)

    def wait(predicate, message):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            read()
            if predicate():
                return
        raise AssertionError(message + '\n' + screen())

    def shown(text):
        wait(lambda: text in screen(), 'Missing UI text: ' + text)

    def send(keys):
        os.write(master, keys)

    try:
        shown('Revisions')
        wait(lambda: current()['commit_id'] != before_open, 'TUI did not snapshot existing edits on opening')
        opened_operation = snapshot()['operation']
        (root / 'startup.txt').write_text('edited after opening\n')
        deadline = time.monotonic() + 2.5
        while time.monotonic() < deadline:
            read()
        assert snapshot()['operation'] == opened_operation, 'auto-snapshot off still recorded subsequent edits'
        send(b'r')
        wait(lambda: screen().splitlines()[-1].startswith('Rebase '), 'rebase status is not in bottom footer')
        assert 'enter apply' in screen().splitlines()[-2]
        send(b'\x1b')
        wait(lambda: not screen().splitlines()[-1].startswith('Rebase '), 'rebase footer did not clear')
        wait(lambda: b'38;2;17;34;51' in output, 'theme key color missing')
        theme.write_text('hint_key="#224466"\n')
        send(b'gt')
        shown('Theme reloaded')
        wait(lambda: b'38;2;34;68;102' in output, 'reloaded theme key color missing')
        send(b'gi')
        shown('Git Operations')
        shown('fetch origin')
        send(b'f')
        shown('Showing: fetch')
        send(b'\r')
        wait(lambda: 'Git Operations' not in screen(), 'Git menu did not close after fetch')
        shown('completed')
        original_op = snapshot()['operation']
        send(b'gn')
        for _ in range(3):
            read(.1)
        assert snapshot()['operation'] == original_op, 'old binding must be disabled'
        old_commit = current()['commit_id']
        send(b'x')
        wait(lambda: current()['commit_id'] != old_commit, 'custom new binding did not execute')
        shown('completed')
        send(b'\r')
        shown('Description')
        send(b'created qnr ; $(literal)\r')
        wait(lambda: current()['description'].strip() == 'created qnr ; $(literal)', 'inline description failed')
        wait(lambda: 'jj describe' in screen() and 'completed' in screen(), 'describe completion not displayed')
        send(b'B')
        shown('▏')
        send(b'topic\r')
        wait(lambda: 'topic' in current()['bookmarks'], 'bookmark creation failed')
        wait(lambda: 'jj bookmark set' in screen() and 'completed' in screen(), 'bookmark completion not displayed')
        send(b'o')
        shown('Operations for change')
        shown('args: jj')
        send(b'o')
        wait(lambda: 'Operations for change' not in screen(), 'inline operation log did not collapse')
        send(b'O')
        shown('Operations ·')
        shown('args: jj')
        (root / 'snapshot-file.txt').write_text('snapshot addition\n')
        jj('util', 'snapshot')
        shown('@  ' + snapshot()['operation'][:12])
        send(b'\x1b[Hd')
        shown('Snapshot diff ·')
        shown('+snapshot addition')
        send(b'h')
        shown('Operations ·')
        send(b'o')
        shown('Operations for change')
        shown('@  ' + snapshot()['operation'][:12])
        send(b'\x1b[Hd')
        shown('Snapshot diff ·')
        shown('+snapshot addition')
        send(b'h')
        shown('Operations for change')
        send(b'h')
        (root / 'file.txt').write_text('chunk first\nmiddle\nchunk last\n')
        jj('util', 'snapshot')
        shown(snapshot()['operation'][:12])
        send(b'l')
        shown('M  file.txt')
        send(b'i')
        shown('Hunks · file.txt')
        shown('+chunk first')
        send(b'vr')
        shown('Restore 1 selected hunk(s)?')
        send(b'\r')
        wait(lambda: (root / 'file.txt').read_text() == 'base file\nmiddle\nchunk last\n', 'partial hunk restore failed')
        shown('Restore selected hunks completed')
        shown('M  file.txt')
        send(b'i')
        shown('+chunk last')
        send(b's')
        shown('Split 1 selected hunk(s): description')
        send(b'split chunk\r')
        shown('Split selected hunks completed')
        assert any(r['description'].strip() == 'split chunk' for r in snapshot()['revisions'])
        assert (root / 'file.txt').read_text() == 'base file\nmiddle\nchunk last\n'
        send(b'hb')
        shown('Bookmark Operations')
        shown('topic')
        send(b'd')
        shown('Showing: delete')
        send(b'\x1b')
        shown('Showing: all actions')
        send(b'm')
        shown('Showing: move')
        send(b'\r')
        wait(lambda: any(r['description'].strip() == 'split chunk' and 'topic' in r['bookmarks'] for r in snapshot()['revisions']), 'bookmark move did not apply')
        wait(lambda: 'Bookmark Operations' not in screen(), 'applied bookmark popup did not close')
        send(b'b')
        shown('Bookmark Operations')
        send(b'\x1b')
        wait(lambda: 'Bookmark Operations' not in screen(), 'bookmark popup did not close')
        send(b'\x14')
        shown('File finder')
        shown('file.txt')
        send(b'\r')
        shown('base file')
        send(b'hL')
        shown('Revision expression')
        send(b'\x01\x1b[3~\x1b[3~\x1b[3~\x1b[3~\x1b[3~@\r')
        shown('revset: @')
        original_op = snapshot()['operation']
        send(b'zd')
        shown('Describe selected revision')
        send(b'\x1b')
        shown('Description cancelled')
        assert snapshot()['operation'] == original_op
        send(b'zd')
        shown('Describe selected revision')
        send(b'roto ; $(literal)\r')
        shown('Description updated by Roto')
        assert current()['description'].strip() == 'roto ; $(literal)'
        send(b'zi')
        shown('Inspect selection')
        send(b'j\r')
        shown('Selected change:')
        send(b'zi')
        shown('Inspect selection')
        send(b'\r')
        shown('--- snapshot-file.txt')
        shown('+snapshot addition')
        send(b'h')
        script = plugins / 'inspect.roto'
        script.write_text(script.read_text().replace('Selected change: ', 'Reloaded selection: '))
        send(b'zi')
        shown('Inspect selection')
        send(b'j\r')
        shown('Reloaded selection:')
        secondary = root / '.jj/secondary'
        send(b'gw')
        shown('Workspaces')
        shown('default (current)')
        send(b'a')
        shown('New workspace path')
        send(str(secondary).encode() + b'\r')
        shown('secondary')
        wait(lambda: secondary.joinpath('.jj').is_dir(), 'workspace not created')
        shown('completed')
        send(b'j\r')
        shown('Switched to ' + str(secondary))
        send(b'i')
        shown('Description')
        send(b'workspace change\r')
        shown('completed')
        assert current()['description'].strip() == 'roto ; $(literal)', 'command targeted the previous workspace'
        send(b'gw')
        shown('secondary (current)')
        send(b'r')
        shown('Rename current workspace')
        send(b'\x15renamed\r')
        shown('renamed (current)')
        (root / 'file.txt').write_text('workspace parent changed\n')
        jj('describe', '-m', 'roto ; $(literal)')
        shown('Working copy STALE')
        shown('[stale]')
        send(b'\x1b')
        wait(lambda: ' Workspaces ' not in screen(), 'workspace menu did not close')
        send(b'gu')
        shown('jj workspace update-stale completed')
        wait(lambda: 'Working copy STALE' not in screen(), 'stale banner survived recovery')
        assert (secondary / 'file.txt').read_text() == 'workspace parent changed\n'
        send(b'gw')
        shown('renamed (current)')
        send(b'k\r')
        shown('Switched to ' + str(root))
        send(b'gw')
        shown('default (current)')
        send(b'jf')
        shown('Forget workspace renamed?')
        send(b'\x1b')
        shown('renamed')
        send(b'f\r')
        shown('completed')
        wait(lambda: 'renamed' not in screen().split('Workspaces')[1].split('switch')[0], 'forgotten workspace remained in menu')
        assert secondary.joinpath('.jj').is_dir(), 'forget deleted files'
        removable = root / '.jj/removable'
        send(b'a')
        shown('New workspace path')
        send(str(removable).encode() + b'\r')
        shown('completed')
        shown('removable')
        send(b'jd')
        shown('DELETE its directory')
        send(b'\x1b')
        assert removable.exists(), 'cancel removed workspace'
        send(b'd\r')
        wait(lambda: not removable.exists(), 'remove left workspace on disk')
        shown('jj workspace remove -- removable completed')
        send(b'\x1b')
        wait(lambda: ' Workspaces ' not in screen(), 'workspace menu did not close')
        jj('bookmark', 'create', 'published', '-r', '@-')
        send(b'L')
        shown('Revision expression')
        send(b'\x15all()\r')
        shown('immutable revision')
        send(b'@J')
        shown('enter show hidden commits')
        before_expand = snapshot()['operation']
        send(b'\r')
        wait(lambda: 'show hidden commits' not in screen(), 'immutable row did not expand')
        assert snapshot()['operation'] == before_expand, 'expanding history mutated the repository'
        assert 'Description' not in screen(), 'expansion opened describe instead'
        shown('esc hide immutable commits')
        send(b'\x1b')
        shown('enter show hidden commits')
        send(b'\r')
        shown('esc hide immutable commits')
        send(b'@')
        wait(lambda: 'hide immutable commits' not in screen(), 'leaving expanded history did not fold it')
        send(b'J')
        shown('enter show hidden commits')
        send(b'@a')
        shown('Confirm abandon')
        assert screen().splitlines()[-1].startswith('Confirm abandon'), 'abandon confirmation is not in bottom footer'
        assert 'enter confirm' in screen().splitlines()[-2]
        send(b'\x1b')
        time.sleep(.1)
        send(b':q')
        wait(lambda: screen().splitlines()[-1].startswith(':q'), 'command input is not on bottom footer line')
        send(b'\r')
        deadline = time.monotonic() + 5
        while process.poll() is None and time.monotonic() < deadline:
            read()
        assert process.wait(timeout=1) == 0
        assert termios.tcgetattr(slave) == original
        print('PTY smoke passed: theme reload, remapping, new/describe, bookmarks, operations, hunk restore/split, file finder, revsets, Roto prompts/commands/diffs/reload, workspaces, immutable history expansion, cancellation, terminal restoration')
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        os.close(slave)
