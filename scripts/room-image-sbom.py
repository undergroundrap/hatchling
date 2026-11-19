"""Generate a deterministic CycloneDX package inventory for the room image."""

import json
import sys
from pathlib import Path
from urllib.parse import quote


components = []
for line in Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    name, version, arch, source, *digests = line.split("\t")
    encoded_version = quote(version, safe="")
    if source == "debian":
        purl = f"pkg:deb/debian/{name}@{encoded_version}?arch={arch}"
    elif source == "pypi":
        purl = f"pkg:pypi/{name}@{encoded_version}"
    else:
        purl = f"pkg:generic/{name}@{encoded_version}?arch={arch}"
    component = {
        "type": "application" if source == "hatchling" else "library",
        "name": name, "version": version, "purl": purl,
    }
    if digests:
        digest, = digests
        component["hashes"] = [{"alg": "SHA-256", "content": digest}]
    components.append(component)

# What the static supervisor was built with (#517): not installed in the
# image, so not its components; nested in the supervisor's, whose binary
# holds glibc and libgcc statically.
if len(sys.argv) > 3:
    built_with = []
    for line in Path(sys.argv[3]).read_text(encoding="utf-8").splitlines():
        name, version, arch, _source = line.split("\t")
        built_with.append({
            "type": "library", "name": name, "version": version,
            "purl": f"pkg:deb/debian/{name}@{quote(version, safe='')}?arch={arch}",
            "scope": "excluded" if name.startswith(("binutils", "gcc-")) else "required",
        })
    supervisor, = (c for c in components if c["name"] == "hatchling-room")
    supervisor["components"] = built_with

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
