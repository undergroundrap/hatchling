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


PAGE = os.sysconf('SC_PAGE_SIZE')

# A pagemap entry's bit for a guard region's page (Linux 6.14+): one
# MADV_GUARD_INSTALL made, as newer glibc does for thread stacks' guards.
GUARD_REGION = 1 << 58


def guard_page(pagemap, address):
    """Whether the page at `address` is a guard region, by its entry in
    the open pagemap `pagemap`: it holds nothing, and faults when read."""
    pagemap.seek(address // PAGE * 8)
    entry = pagemap.read(8)
    return len(entry) == 8 and bool(int.from_bytes(entry, 'little') & GUARD_REGION)


def readable_now(pid, address):
    """Whether `address` is in a readable mapping of process `pid` now.

    The scan reads a running process: a thread that ends frees its stack,
    and a page listed when the scan began can be gone when it's read.
    """
    with open(f'/proc/{pid}/maps', encoding='utf-8') as maps:
        for line in maps:
            fields = line.split()
            start, end = (int(bound, 16) for bound in fields[0].split('-'))
            if start <= address < end:
                return fields[1].startswith('r')
    return False


def page_flags(pagemap, address):
    """The flag bits (55 to 63) of the page at `address`'s pagemap entry:
    never its frame or swap number."""
    pagemap.seek(address // PAGE * 8)
    entry = pagemap.read(8)
    return int.from_bytes(entry, 'little') >> 55 << 55 if len(entry) == 8 else 0


def read_range(mem, pagemap, offset, size, mapping, still_readable):
    """The bytes at [offset, offset + size) of a process, with the number
    read: a guard region's page, or one no longer mapped readable
    (`still_readable(address)` says), reads as zeros, and isn't counted.

    Any other page that won't read fails the scan, naming the mapping by
    its permissions and name and the page's pagemap flags, never what it
    holds.
    """
    mem.seek(offset)
    try:
        data = mem.read(size)
        return data, len(data)
    except OSError:
        pass
    data, read = bytearray(), 0
    for page in range(offset, offset + size, PAGE):
        length = min(PAGE - page % PAGE, offset + size - page)
        mem.seek(page)
        try:
            chunk = mem.read(length)
        except OSError as error:
            if not guard_page(pagemap, page) and still_readable(page):
                flags = page_flags(pagemap, page)
                raise OSError(error.errno, f'{error.strerror} reading the mapping {mapping}'
                              f' (pagemap flags {flags:#x}, kernel {os.uname().release})') from error
            chunk = bytes(length)
        else:
            read += len(chunk)
        data += chunk
    return bytes(data), read


def memory_holds(pid, key, chunk_size=1024 * 1024):
    """Whether any readable mapping of a running process holds the key.

    Returns that and the bytes read: a scan that read nothing proves
    nothing. Only the verdict leaves this function, never what was read.
    """
    if not key or chunk_size < len(key):
        raise ValueError('Expected a nonempty test credential shorter than a chunk')
    held, scanned = False, 0
    with (open(f'/proc/{pid}/maps', encoding='utf-8') as maps,
          open(f'/proc/{pid}/mem', 'rb', 0) as mem,
          open(f'/proc/{pid}/pagemap', 'rb', 0) as pagemap):
        for line in maps:
            fields = line.split()
            start, end = (int(bound, 16) for bound in fields[0].split('-'))
            # The kernel's own pages ([vvar], [vsyscall]) can't be read
            # through mem; nothing of the process's is there.
            if not fields[1].startswith('r') or fields[-1] in ('[vvar]', '[vvar_vclock]', '[vsyscall]'):
                continue
            previous = b''
            mapping = f"{fields[1]} {' '.join(fields[5:]) or '(anonymous)'}"
            for offset in range(start, end, chunk_size):
                data, read = read_range(mem, pagemap, offset, min(chunk_size, end - offset), mapping,
                                        lambda address: readable_now(pid, address))
                held = held or key in previous + data
                scanned += read
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
