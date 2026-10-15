"""Authenticated HTTPS CI stand-in; generated credentials stay outside the clone."""

import argparse
import hmac
from http.server import HTTPServer
import importlib.util
import io
import json
from pathlib import Path
import ssl
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('repo', 'key-file', 'cert-file', 'private-key-file', 'armed', 'release'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--port', type=int, required=True)
    args = parser.parse_args()
    key = args.key_file.read_bytes().decode('ascii')
    if not key:
        raise ValueError('Empty test credential')
    spec = importlib.util.spec_from_file_location(
        'readme_model', args.repo / 'docs/examples/stand-in-model.py')
    model = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(model)

    class Handler(model.Handler):
        def authorized(self):
            if hmac.compare_digest(self.headers.get('Authorization', ''), 'Bearer ' + key):
                return True
            self.send_error(401, 'API key required')
            return False

        def do_GET(self):
            if self.path == '/health' or self.authorized():
                self.send_response(200)
                self.end_headers()

        def do_POST(self):
            if not self.authorized():
                return
            # Prechecks run before this barrier is armed. Only the real agent
            # waits for a complete live-cell inspection.
            if args.armed.exists():
                deadline = time.monotonic() + 20
                while not args.release.exists():
                    if time.monotonic() >= deadline:
                        self.send_error(503, 'Live-cell inspection deadline')
                        return
                    time.sleep(0.01)
            size = int(self.headers.get('Content-Length', '0'))
            if not 0 < size < 1024 * 1024:
                self.send_error(400)
                return
            body = self.rfile.read(size)
            if 'response_format' in json.loads(body):
                self.send_error(400, 'Unexpected response_format')
                return
            self.rfile = io.BytesIO(body)
            super().do_POST()

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    # Like a real provider: nothing older than TLS 1.2.
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(args.cert_file, args.private_key_file)
    server = HTTPServer(('127.0.0.1', args.port), Handler)
    server.socket = context.wrap_socket(server.socket, server_side=True)
    server.serve_forever()


if __name__ == '__main__':
    main()
