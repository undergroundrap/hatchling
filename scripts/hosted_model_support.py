"""Nonsecret, fail-closed snapshot helpers for the hosted-model CI proof."""

import hashlib
import os
from pathlib import Path, PurePosixPath
import re
import stat


class CredentialLeak(ValueError):
    """All logs were checked/redacted, but the proof must still fail."""


def checked_file(path, key, chunk_size=1024 * 1024):
    """Hash every byte and find credentials even across read boundaries."""
    if not key or chunk_size <= 0:
        raise ValueError('Expected a nonempty test credential and positive chunk size')
    digest = hashlib.sha256()
    previous = b''
    count = 0
    with path.open('rb') as stream:
        while data := stream.read(chunk_size):
            if key in previous + data:
                raise ValueError('Test credential appeared in a cell-view file')
            digest.update(data)
            count += len(data)
            previous = (previous + data)[-(len(key) - 1):] if len(key) > 1 else b''
    return {'bytes': count, 'sha256': digest.hexdigest()}


def mount_unescape(value):
    return re.sub(r'\\([0-7]{3})', lambda match: chr(int(match.group(1), 8)), value)


def readonly_mount(path, mountinfo):
    """Require the longest matching mount to be readonly, including submounts."""
    matches = []
    path = PurePosixPath('/' + str(path))
    for line in mountinfo.splitlines():
        fields = line.split(' ')
        mount = PurePosixPath(mount_unescape(fields[4]))
        if path == mount or mount in path.parents:
            matches.append((len(mount.parts), fields[5].split(',')))
    deepest = max((depth for depth, _ in matches), default=-1)
    if not matches or any('ro' not in options for depth, options in matches if depth == deepest):
        raise ValueError('Cell-view file is not on a readonly mount')


def snapshot(proc, key, program, agent_hash, forbidden):
    """Inspect an already identified process through its pinned proc directory.

    The result describes the initial exec environment and filesystem snapshot;
    it does not claim to inspect arbitrary process memory or every instant.
    """
    environment = (proc / 'environ').read_bytes()
    argv = (proc / 'cmdline').read_bytes()
    if key in environment or key in argv:
        raise ValueError('Test credential appeared in the exec environment or argv')
    root = proc / 'root'
    for path in forbidden:
        if (root / path.relative_to('/')).exists():
            raise ValueError('Host credential storage appeared in the cell view')
    mountinfo = (proc / 'mountinfo').read_text(encoding='utf-8')
    entries = []
    def traversal_error(error):
        raise error
    for directory, dirs, files in os.walk(root, followlinks=False, onerror=traversal_error):
        for name in dirs:
            if Path(directory, name).is_symlink():
                raise ValueError('Unexpected directory link in the agent view')
        for name in files:
            path = Path(directory, name)
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode):
                raise ValueError('Unexpected file link in the agent view')
            if not stat.S_ISREG(mode):
                continue
            relative = path.relative_to(root)
            readonly_mount(relative.as_posix(), mountinfo)
            entries.append(dict(path=relative.as_posix(), readonly=True,
                                **checked_file(path, key)))
    executable = next((entry for entry in entries if entry['path'] == program.lstrip('/')), None)
    if executable is None or executable['sha256'] != agent_hash or len(entries) < 3:
        raise ValueError('Incomplete view or unexpected agent executable')
    return {'environment_entries': len([entry for entry in environment.split(b'\0') if entry]),
            'initial_environment_argv_and_view_key_absent': True, 'files': entries}


def memory_holds(pid, key, chunk_size=1024 * 1024):
    """Whether any readable mapping of a running process holds the key.

    Returns that and the bytes read: a scan that read nothing proves
    nothing. Only the verdict leaves this function, never what was read.
    """
    if not key or chunk_size < len(key):
        raise ValueError('Expected a nonempty test credential shorter than a chunk')
    held, scanned = False, 0
    with open(f'/proc/{pid}/maps', encoding='utf-8') as maps, open(f'/proc/{pid}/mem', 'rb', 0) as mem:
        for line in maps:
            fields = line.split()
            start, end = (int(bound, 16) for bound in fields[0].split('-'))
            # The kernel's own pages ([vvar], [vsyscall]) can't be read
            # through mem; nothing of the process's is there.
            if not fields[1].startswith('r') or fields[-1] in ('[vvar]', '[vvar_vclock]', '[vsyscall]'):
                continue
            previous = b''
            for offset in range(start, end, chunk_size):
                mem.seek(offset)
                data = mem.read(min(chunk_size, end - offset))
                held = held or key in previous + data
                scanned += len(data)
                previous = data[-(len(key) - 1):] if len(key) > 1 else b''
    return held, scanned


def sanitize_logs(artifacts, key):
    """Protect retained logs while preserving a failing credential-leak verdict."""
    contaminated = []
    for path in artifacts.glob('*.log'):
        data = path.read_bytes()
        if key in data:
            path.write_bytes(data.replace(key, b'<redacted-test-credential>'))
            contaminated.append(path.name)
    if contaminated:
        (artifacts / 'SANITIZED.txt').write_text(
            'FAILED: generated test credential was redacted from these logs:\n'
            + '\n'.join(contaminated) + '\n', encoding='utf-8')
        raise CredentialLeak('Test credential appeared in retained logs; proof failed')
