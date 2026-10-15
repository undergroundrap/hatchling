"""Fetch SHA-256-pinned, test-only llama.cpp and Qwen fixtures outside the clone."""

import argparse
import hashlib
import json
from pathlib import Path
import tarfile
import urllib.request


def download(pin, destination):
    digest = hashlib.sha256()
    count = 0
    with urllib.request.urlopen(pin['url'], timeout=120) as response, destination.open('xb') as output:
        while chunk := response.read(1024 * 1024):
            count += len(chunk)
            if count > pin['bytes']:
                raise ValueError('Fixture exceeds pinned size')
            digest.update(chunk)
            output.write(chunk)
    if count != pin['bytes'] or digest.hexdigest() != pin['sha256']:
        raise ValueError('Fixture does not match its pinned size and SHA-256')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    destination = args.destination.resolve()
    repo = Path(__file__).resolve().parents[1]
    if destination == repo or repo in destination.parents:
        parser.error('Fixtures belong outside the checkout')
    destination.mkdir(mode=0o700, parents=True, exist_ok=False)
    pins = json.loads(Path(__file__).with_name('hosted-model-pins.json').read_text(encoding='utf-8'))
    archive = destination / 'llama.tar.gz'
    download(pins['llama_cpp'], archive)
    download(pins['model'], destination / 'model.gguf')
    # Python 3.12+ data filter rejects traversal, device entries and unsafe links.
    with tarfile.open(archive) as package:
        package.extractall(destination / 'llama', filter='data')
    servers = list((destination / 'llama').rglob('llama-server'))
    if len(servers) != 1 or not servers[0].is_file():
        raise ValueError('Expected one pinned llama-server')
    (destination / 'paths.json').write_text(json.dumps({
        'server': str(servers[0]), 'model': str(destination / 'model.gguf')}, indent=2) + '\n', encoding='utf-8')
    print('Downloaded and verified both test fixtures')


if __name__ == '__main__':
    main()
