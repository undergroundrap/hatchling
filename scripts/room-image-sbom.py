"""Generate a deterministic CycloneDX package inventory for the room image."""

import json
import sys
from pathlib import Path
from urllib.parse import quote


components = []
for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    name, version, arch, source = line.split("\t")
    encoded_version = quote(version, safe="")
    if source == "debian":
        purl = f"pkg:deb/debian/{name}@{encoded_version}?arch={arch}"
    elif source == "pypi":
        purl = f"pkg:pypi/{name}@{encoded_version}"
    else:
        purl = f"pkg:generic/{name}@{encoded_version}?arch={arch}"
    components.append({"type": "library", "name": name, "version": version, "purl": purl})

print(json.dumps({
    "bomFormat": "CycloneDX",
    "specVersion": "1.6",
    "version": 1,
    "metadata": {"component": {
        "type": "operating-system",
        "name": "Hatchling room system image",
        "hashes": [{"alg": "SHA-256", "content": sys.argv[2]}],
    }},
    "components": components,
}, indent=2))
