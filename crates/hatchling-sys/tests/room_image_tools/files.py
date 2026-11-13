"""Files, processes and archives, as a script of an agent's might use them."""

import asyncio
import csv
import glob
import hashlib
import json
import os
import pathlib
import shutil
import sqlite3
import subprocess
import tarfile
import tempfile
import threading
import time
import zipfile
from concurrent.futures import ThreadPoolExecutor


async def later():
    await asyncio.sleep(0.01)
    proc = await asyncio.create_subprocess_exec("true")
    await proc.wait()


shutil.copytree("dir", "py-copy", symlinks=True)
shutil.copy2("numbers.txt", "py-copy/n2.txt")
shutil.move("py-copy/n2.txt", "py-n2.txt")
shutil.move("py-n2.txt", "/session/py-n2.txt")
for _root, _dirs, _files in os.walk("."):
    pass
with tempfile.TemporaryDirectory() as d:
    pathlib.Path(d, "a").write_text("a")
with tempfile.NamedTemporaryFile() as f:
    f.write(b"a")
    f.flush()
    os.fsync(f.fileno())
assert subprocess.run(["true"], capture_output=True, check=True).returncode == 0
assert subprocess.check_output(["sh", "-c", "echo a | tr a b"]) == b"b\n"
piped = subprocess.Popen(["cat"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
assert piped.communicate(b"x")[0] == b"x"
t = threading.Thread(target=abs, args=(1,))
t.start()
t.join()
with ThreadPoolExecutor(4) as pool:
    assert list(pool.map(abs, [1, -2])) == [1, 2]
asyncio.run(later())
db = sqlite3.connect("py.db")
db.execute("create table t (a)")
db.execute("insert into t values (1)")
db.commit()
db.close()
with zipfile.ZipFile("py.zip", "w") as z:
    z.write("numbers.txt")
with tarfile.open("py.tar.gz", "w:gz") as tf:
    tf.add("dir")
with tarfile.open("py.tar.gz") as tf:
    tf.extractall("py-out", filter="data")
with open("py.csv", "w", newline="") as f:
    csv.writer(f).writerows([[1, 2], [3, 4]])
print(hashlib.sha256(pathlib.Path("numbers.txt").read_bytes()).hexdigest(), json.dumps(sorted(glob.glob("*.csv"))))
print(time.time() > 0, os.getpid() > 0, os.cpu_count() > 0, os.statvfs(".").f_bavail > 0)
shutil.rmtree("py-copy")
