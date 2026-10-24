"""Prove vault-backed model access through a real scheduled agent cell.

Run only as root PID 1 in disposable mount/PID/network namespaces:
sudo unshare --mount --pid --fork --kill-child --mount-proc --net \
  python3 scripts/hosted_model_smoke.py REPO ARTIFACTS EMPTY_WORK --mode https
For --mode local also supply --fixtures PATH_TO_VERIFIED_PATHS_JSON.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pwd
import re
import secrets
import shutil
import signal
import ssl
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request

from hosted_model_support import CredentialLeak, sanitize_logs, snapshot
from readme_quickstart import expected_answer, git, require_answer


def require(condition, message):
    if not condition:
        raise ValueError(message)


def private_file(path, data):
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'wb') as stream:
        stream.write(data)


def isolate(work):
    """Only this mount namespace sees the test account, hosts entry and keyring."""
    subprocess.run(['mount', '--make-rprivate', '/'], check=True)
    for name in ('upper', 'overlay-work'):
        (work / name).mkdir()
    subprocess.run(['mount', '-t', 'overlay', 'overlay', '-o',
                    f'lowerdir=/etc,upperdir={work}/upper,workdir={work}/overlay-work', '/etc'], check=True)
    for path in ('/run', '/tmp', '/root'):
        subprocess.run(['mount', '-t', 'tmpfs', 'tmpfs', path], check=True)
    subprocess.run(['ip', 'link', 'set', 'lo', 'up'], check=True)


def certificates(work):
    def openssl(*args):
        subprocess.run(['openssl', *map(str, args)], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    openssl('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
            '-subj', '/CN=Hatchling disposable CI CA', '-keyout', work / 'ca.key',
            '-out', work / 'ca.crt', '-addext', 'basicConstraints=critical,CA:TRUE',
            '-addext', 'keyUsage=critical,keyCertSign,cRLSign')
    openssl('req', '-new', '-newkey', 'rsa:2048', '-nodes', '-subj', '/CN=api.example.test',
            '-keyout', work / 'server.key', '-out', work / 'server.csr')
    (work / 'server.ext').write_text(
        'subjectAltName=DNS:api.example.test\nbasicConstraints=critical,CA:FALSE\n'
        'keyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n', encoding='utf-8')
    openssl('x509', '-req', '-in', work / 'server.csr', '-CA', work / 'ca.crt',
            '-CAkey', work / 'ca.key', '-CAcreateserial', '-days', '1',
            '-extfile', work / 'server.ext', '-out', work / 'server.crt')
    for name in ('ca.key', 'server.key'):
        (work / name).chmod(0o600)
    with Path('/etc/hosts').open('a', encoding='utf-8') as hosts:
        hosts.write('\n127.0.0.1 api.example.test\n')


def stopped_server(pid):
    states = [(item / 'status').read_text() for item in Path(f'/proc/{pid}/task').iterdir()]
    return bool(states) and all(re.search(r'^State:\s+T', state, re.M) for state in states)


def start_time(proc):
    return (proc / 'stat').read_text().rsplit(')', 1)[1].split()[19]


def reap_namespace():
    """End descendants which inherited log descriptors before scanning logs.

    This runner is PID 1; /proc was mounted by unshare for this PID namespace.
    Namespace exit is the outer fallback, including children in new sessions.
    """
    require(os.getpid() == 1, 'Namespace reaping requires the isolated PID 1 runner')
    for sig in (signal.SIGTERM, signal.SIGKILL):
        for item in Path('/proc').iterdir():
            if item.name.isdigit() and int(item.name) != 1:
                try:
                    os.kill(int(item.name), sig)
                except ProcessLookupError:
                    pass
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            try:
                pid, _ = os.waitpid(-1, os.WNOHANG)
            except ChildProcessError:
                return
            if not pid:
                time.sleep(0.01)
    raise ValueError('Namespace descendants did not stop; do not retain unchecked logs')


def inspect_agent(uid, key, program, agent_hash, endpoint, model, forbidden, task, stop, observations, errors, release):
    """The model response is held until this complete, identity-bound snapshot."""
    try:
        while not stop.wait(0.005):
            for item in Path('/proc').iterdir():
                if not item.name.isdigit():
                    continue
                fd = None
                identified = False
                try:
                    fd = os.open(item, os.O_RDONLY | os.O_DIRECTORY)
                    proc = Path(f'/proc/self/fd/{fd}')
                    if os.readlink(proc / 'exe') != program:
                        continue
                    if (proc / 'cmdline').read_bytes().split(b'\0') != [program.encode(), b'run', task.encode(), b'']:
                        continue
                    uid_line = next(line for line in (proc / 'status').read_text().splitlines() if line.startswith('Uid:'))
                    if set(map(int, uid_line.split()[1:])) != {uid}:
                        continue
                    identified = True
                    began = time.monotonic()
                    identity = start_time(proc)
                    env = dict(entry.split(b'=', 1) for entry in (proc / 'environ').read_bytes().split(b'\0') if entry)
                    require(env.get(b'HATCHLING_MODEL_ENDPOINT') == endpoint.encode()
                            and env.get(b'HATCHLING_MODEL_NAME') == model.encode(), 'Unexpected agent model configuration')
                    run_id = env.get(b'HATCHLING_CELL_ID', b'').decode('ascii')
                    require(re.fullmatch(r'run-[0-9a-f]+', run_id), 'Expected scheduled run identity')
                    observation = snapshot(proc, key, program, agent_hash, forbidden)
                    require(start_time(proc) == identity and os.readlink(proc / 'exe') == program,
                            'Agent identity changed during inspection')
                    require(time.monotonic() - began < 10, 'Cell inspection exceeded ten seconds')
                    observations.append(dict(pid=int(item.name), host_uid=uid, run_id=run_id, **observation))
                    release()
                    return
                except (FileNotFoundError, ProcessLookupError):
                    if identified:
                        raise ValueError('Identified agent disappeared before inspection completed')
                except PermissionError:
                    # Another process, mid-exec or changing credentials, can be
                    # briefly unreadable: skip it this pass. The agent, once
                    # identified, must stay readable throughout.
                    if identified:
                        raise
                finally:
                    if fd is not None:
                        os.close(fd)
    except Exception as error:
        errors.append(type(error).__name__ + ': ' + str(error))


def finalize(artifacts, key, children, logs, observer, server_fd, stop):
    cleanup_errors = []
    def attempt(label, action):
        try:
            action()
            return True
        except BaseException as error:
            cleanup_errors.append(label + ': ' + type(error).__name__)
            return False
    stop.set()
    # Resume through the owned pidfd even when stop confirmation or a later
    # assertion failed; the stopped model must never outlive a failing proof.
    if server_fd is not None:
        def resume():
            try:
                signal.pidfd_send_signal(server_fd, signal.SIGCONT)
            except ProcessLookupError:
                pass
        attempt('resume model', resume)
    if observer is not None:
        attempt('join observer', lambda: observer.join(timeout=15))
        if observer.is_alive():
            cleanup_errors.append('observer did not stop')
    for child in reversed(children):
        def end_child():
            if child.poll() is None:
                try:
                    child.terminate()
                except ProcessLookupError:
                    pass
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    try:
                        child.kill()
                    except ProcessLookupError:
                        pass
                    child.wait(timeout=5)
        attempt('stop owned child', end_child)
    attempt('reap namespace', reap_namespace)
    if server_fd is not None:
        attempt('close model pidfd', lambda: os.close(server_fd))
    for log in logs:
        attempt('close log writer', log.close)
    if cleanup_errors:
        for path in artifacts.glob('*.log'):
            path.unlink()
        (artifacts / 'WITHHELD.txt').write_text(
            'FAILED: logs withheld after cleanup errors:\n' + '\n'.join(cleanup_errors) + '\n', encoding='utf-8')
        raise ValueError('Proof cleanup failed; logs withheld: ' + str(cleanup_errors))
    try:
        sanitize_logs(artifacts, key)
    except CredentialLeak:
        raise
    except BaseException as error:
        raise ValueError('Credential log check could not complete; logs withheld') from error


def finish_logs(pending, artifacts, key, children, logs, observer, server_fd, stop):
    leak = None
    try:
        finalize(pending, key, children, logs, observer, server_fd, stop)
    except CredentialLeak as error:
        # Raised only after checking/redacting every log and writing its label.
        leak = error
    except BaseException:
        (artifacts / 'WITHHELD.txt').write_text(
            'FAILED: logs stayed in private staging because safe finalization failed.\n', encoding='utf-8')
        raise
    for path in pending.iterdir():
        shutil.copy2(path, artifacts / path.name)
    if leak is not None:
        raise leak


def run(repo, artifacts, work, mode, fixtures):
    require(os.geteuid() == 0 and os.getpid() == 1, 'Use root in disposable namespaces (see module usage)')
    require(not git(repo, 'status', '--porcelain'), 'Proof requires a clean committed checkout')
    require(work.is_dir() and not any(work.iterdir()), 'Work directory must be fresh and empty')
    require(repo not in work.parents and repo != work, 'Private state must be outside the checkout')
    work.chmod(0o700)
    artifacts.mkdir(parents=True, exist_ok=False)
    source = {'commit': git(repo, 'rev-parse', 'HEAD'), 'tree': git(repo, 'rev-parse', 'HEAD^{tree}')}
    (artifacts / 'source.json').write_text(json.dumps(source, indent=2) + '\n', encoding='utf-8')
    isolate(work)
    # Unchecked bytes never live under the directory uploaded by CI. Even an
    # I/O failure during redaction/withholding cannot expose a later log there.
    pending = work / 'pending-logs'
    pending.mkdir(mode=0o700)
    try:
        pwd.getpwnam('hatchling-hosted-ci')
    except KeyError:
        pass
    else:
        raise ValueError('Refuse to reuse an existing test account')
    subprocess.run(['useradd', '--system', '--user-group', '--no-create-home', '--no-log-init',
                    '--shell', '/usr/sbin/nologin', 'hatchling-hosted-ci'], check=True)
    uid = pwd.getpwnam('hatchling-hosted-ci').pw_uid
    # As root the broker runs its vault keeper as a user of its own.
    subprocess.run(['useradd', '--system', '--user-group', '--no-create-home', '--no-log-init',
                    '--shell', '/usr/sbin/nologin', 'hatchling-keeper'], check=True)
    # /tmp is a private tmpfs above. A short state path also keeps broker/run
    # Unix sockets below the kernel limit, regardless of RUNNER_TEMP length.
    state = Path('/tmp/hatchling-hosted')
    state.mkdir(mode=0o700)
    # Searchable, not readable: the keeper's user reaches the vault inside.
    state.chmod(0o711)
    for name in ('sock', 'vault', 'schedules'):
        (state / name).mkdir(mode=0o700)
    key = ('hatchling-ci-' + secrets.token_urlsafe(32)).encode('ascii')
    key_file = work / 'model-key'
    private_file(key_file, key)
    destination = ('localhost' if mode == 'local' else 'api.example.test') + ':18090'
    endpoint = ('http://' if mode == 'local' else 'https://') + destination + '/v1'
    model = 'qwen3-0.6b'
    secret_file = state / 'vault' / ('egress-' + re.sub(r'[^A-Za-z0-9]', '-', destination))
    private_file(secret_file, key)
    # The cell's mapped UID cannot traverse a maintainer's private home. Keep
    # only the built programs on a readable path in this namespace's tmpfs.
    binaries = Path('/tmp/hatchling-hosted-bin')
    binaries.mkdir(mode=0o755)
    binaries.chmod(0o755)
    for name in ('hatchling-broker', 'hatchling-agent'):
        shutil.copy2(repo / 'target/debug' / name, binaries / name)
        (binaries / name).chmod(0o755)
    broker_path = binaries / 'hatchling-broker'
    agent_path = binaries / 'hatchling-agent'
    agent_hash = hashlib.sha256(agent_path.read_bytes()).hexdigest()
    program = re.search(r'pub const PROGRAM: &str = "([^"]+)";',
                        (repo / 'crates/hatchling-isolation/src/lib.rs').read_text()).group(1)
    env = {name: value for name, value in os.environ.items() if not name.startswith('HATCHLING_')}
    env.update(HATCHLING_BROKER_SOCKET=str(state / 'sock/broker.sock'), HATCHLING_AGENT_UID=str(uid),
               HATCHLING_CELL_UID_BASE=str(uid), HATCHLING_AGENT_BIN=str(agent_path), HATCHLING_CELL_ID='cell-01',
               HATCHLING_VAULT_DIR=str(state / 'vault'), HATCHLING_SCHEDULER_DIR=str(state / 'schedules'),
               HATCHLING_CHANNEL_INGRESS=str(state / 'ingress.sock'), HATCHLING_API_SOCKET=str(state / 'api.sock'),
               HATCHLING_SCHEDULER_MODEL_ENDPOINT=endpoint, HATCHLING_SCHEDULER_MODEL_NAME=model,
               HATCHLING_EGRESS_ALLOWLIST=('local:' if mode == 'local' else 'internal:') + destination,
               HATCHLING_EGRESS_GRANTS=destination, HATCHLING_API_GRANTS='egress:' + destination + ',channel-send:local',
               NO_COLOR='1', PYTHONDONTWRITEBYTECODE='1', PYTHONUNBUFFERED='1')
    if mode == 'https':
        certificates(work)
        env['HATCHLING_EGRESS_CA_BUNDLE'] = str(work / 'ca.crt')
        command = [sys.executable, str(repo / 'scripts/https_model_fixture.py'), '--repo', str(repo),
                   '--key-file', str(key_file), '--cert-file', str(work / 'server.crt'),
                   '--private-key-file', str(work / 'server.key'), '--port', '18090',
                   '--armed', str(work / 'armed'), '--release', str(work / 'release')]
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
                    urllib.request.HTTPSHandler(context=ssl.create_default_context(cafile=work / 'ca.crt')))
    else:
        require(fixtures is not None, 'Local proof needs verified fixture paths')
        paths = json.loads(fixtures.read_text(encoding='utf-8'))
        command = [paths['server'], '--model', paths['model'], '--alias', model, '--host', '127.0.0.1',
                   '--port', '18090', '--api-key-file', str(key_file), '--threads', '2', '--threads-batch', '2',
                   '--ctx-size', '8192', '--parallel', '1', '--n-predict', '256', '--seed', '0', '--temp', '0',
                   '--reasoning', 'off', '--no-webui', '--no-slots']
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def request(path, credential=None, body=None):
        headers = {'Content-Type': 'application/json'}
        if credential is not None:
            headers['Authorization'] = 'Bearer ' + credential.decode('ascii')
        req = urllib.request.Request(endpoint.removesuffix('/v1') + path,
                headers=headers, data=json.dumps(body).encode() if body is not None else None)
        try:
            with opener.open(req, timeout=5) as response:
                return response.status
        except urllib.error.HTTPError as error:
            return error.code
    children, logs, observations, errors = [], [], [], []
    stop = threading.Event()
    observer = None
    server_fd = None
    def release():
        if server_fd is not None:
            signal.pidfd_send_signal(server_fd, signal.SIGCONT)
        else:
            (work / 'release').touch(mode=0o600)
    def launch(name, command):
        log = (pending / (name + '.log')).open('x')
        logs.append(log)
        child = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
        children.append(child)
        return child
    failure = None
    try:
        migration = launch('migration', [str(broker_path), 'migrate-vault'])
        require(migration.wait(timeout=30) == 0, 'Vault migration failed')
        envelope = secret_file.read_bytes()
        require(envelope.startswith(b'HVL3') and key not in envelope, 'Vault entry was not sealed')
        require(secret_file.stat().st_mode & 0o777 == 0o600 and (state / 'vault').stat().st_mode & 0o777 == 0o700,
                'Unsafe vault permissions')
        server = launch('model', command)
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            require(server.poll() is None, 'Model server exited before readiness')
            try:
                if request('/health') == 200:
                    break
            except urllib.error.URLError:
                pass
            time.sleep(0.2)
        else:
            raise ValueError('Model readiness deadline')
        auth = {}
        body = {'model': model, 'messages': [{'role': 'user', 'content': 'Hello'}], 'max_tokens': 16}
        for label, credential in [('missing', None), ('wrong', b'wrong-test-key')]:
            for path, payload in [('/v1/models', None), ('/v1/chat/completions', body)]:
                status = request(path, credential, payload)
                require(status == 401, 'Model accepted an unauthenticated request')
                auth[label + path] = status
        require(request('/v1/models', key) == 200, 'Model refused the correct key')
        require(request('/v1/chat/completions', key, body) == 200, 'Model refused authenticated chat')
        if mode == 'local':
            server_fd = os.pidfd_open(server.pid)
            signal.pidfd_send_signal(server_fd, signal.SIGSTOP)
            deadline = time.monotonic() + 5
            while not stopped_server(server.pid):
                require(time.monotonic() < deadline, 'Model stop confirmation deadline')
                time.sleep(0.01)
        else:
            (work / 'armed').touch(mode=0o600)
        broker = launch('broker', [str(broker_path)])
        deadline = time.monotonic() + 15
        while not (state / 'api.sock').is_socket():
            require(broker.poll() is None and time.monotonic() < deadline, 'Broker startup failed')
            time.sleep(0.01)
        # Read the actual documented task constant, without duplicating its text.
        sys.path.insert(0, str(repo / 'docs/examples'))
        spec = importlib.util.spec_from_file_location('first_run', repo / 'docs/examples/first-run.py')
        example = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(example)
        observer = threading.Thread(target=inspect_agent, daemon=True, args=(uid, key, program, agent_hash,
            endpoint, model, [key_file, state / 'vault'], example.TASK, stop, observations, errors, release))
        observer.start()
        client = launch('client', [sys.executable, str(repo / 'docs/examples/first-run.py'),
                                   str(state / 'api.sock')])
        deadline = time.monotonic() + 350
        while client.poll() is None:
            require(not errors, 'Live-agent inspection failed: ' + str(errors))
            require(time.monotonic() < deadline, 'Documented client exceeded its answer deadline')
            time.sleep(0.05)
        require(client.returncode == 0, 'Documented client did not receive an agent answer')
        stop.set()
        observer.join(timeout=15)
        require(not observer.is_alive() and not errors and len(observations) == 1,
                'Missing or incomplete live-agent inspection: ' + str(errors))
        for log in logs:
            log.flush()
        answer = (pending / 'client.log').read_text(encoding='utf-8')
        require(any(line.startswith('agent: ') and line[7:].strip() for line in answer.splitlines()), 'No agent answer')
        if mode == 'https':
            require_answer([answer], expected_answer(repo / 'docs/examples/stand-in-model.py'))
        broker_text = re.sub(r'\x1b\[[0-9;]*m', '', (pending / 'broker.log').read_text(encoding='utf-8'))
        observed = observations[0]
        require(any('agent session established' in line and f'peer_uid={uid}' in line
                    and f'cell_id={observed["run_id"]}' in line for line in broker_text.splitlines()),
                'Inspected run has no matching broker session')
        require(sum('egress request forwarded' in line and f'peer_uid={uid}' in line
                    and f'destination={destination}' in line and 'method=POST' in line and 'status=200' in line
                    for line in broker_text.splitlines()) >= 2, 'Missing authenticated model forwards')
        proof = dict(mode=mode, source=source, unauthorized=auth, correct_key_models_and_chat=200,
                     vault_envelope='HVL3', vault_mode='700', sealed_file_mode='600',
                     agent_answer_arrived=True, observations=observations)
    except BaseException as error:
        failure = error
    finally:
        try:
            finish_logs(pending, artifacts, key, children, logs, observer, server_fd, stop)
        except BaseException as error:
            raise error from failure
    if failure is not None:
        raise failure
    require(not git(repo, 'status', '--porcelain') and git(repo, 'rev-parse', 'HEAD') == source['commit']
            and git(repo, 'rev-parse', 'HEAD^{tree}') == source['tree'], 'Source changed during the proof')
    (artifacts / 'proof.json').write_text(json.dumps(proof, indent=2) + '\n', encoding='utf-8')
    print('Hosted-model ' + mode + ': passed; sealed key, inspected real cell, agent answer', flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('repo', 'artifacts', 'work'):
        parser.add_argument(name, type=Path)
    parser.add_argument('--mode', choices=('local', 'https'), required=True)
    parser.add_argument('--fixtures', type=Path)
    args = parser.parse_args()
    try:
        run(args.repo.resolve(), args.artifacts.resolve(), args.work.resolve(), args.mode, args.fixtures)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, 'Hosted-model proof: ' + str(error) + '\n')


if __name__ == '__main__':
    main()
