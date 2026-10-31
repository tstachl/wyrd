#!/usr/bin/env python3
# nix/microvm/validate-manifest.py — build-time gate.image/v1 check.
#
# Usage: validate-manifest.py <manifest.json> <schema.json>
# Exits 0 with the schema id on success; exits 1 naming the field
# and the schema path on the first violation. Every bound enforced
# here is read from the schema file (kind const, required key set,
# per-field types/patterns/minima/maxima). A manifest that violates
# the vendored schema fails the build; a gate-side schema change the
# vendored copy does not have is invisible here and is caught by the
# host at launch instead. Invoked by the wyrdImage derivation in
# nix/microvm/image.nix against the vendored
# nix/microvm/image-manifest.schema.json.
import json
import re
import sys


def fail(field, msg, schema_path):
    print("manifest invalid: %s: %s (schema: %s)" % (field, msg, schema_path))
    raise SystemExit(1)


def main():
    manifest_path, schema_path = sys.argv[1], sys.argv[2]
    manifest = json.load(open(manifest_path))
    schema = json.load(open(schema_path))
    props = schema["properties"]

    if set(manifest) != set(schema["required"]):
        fail("(keys)", "keys are not exactly %s" % sorted(schema["required"]), schema_path)

    for key in schema["required"]:
        spec, val = props[key], manifest[key]
        if "const" in spec:
            if val != spec["const"]:
                fail(key, "is not %r" % spec["const"], schema_path)
        t = spec.get("type")
        if t is None:
            continue
        if t == "string":
            if not isinstance(val, str):
                fail(key, "is not a string", schema_path)
        elif t == "integer":
            if not isinstance(val, int) or isinstance(val, bool):
                fail(key, "is not an integer", schema_path)
        elif t == "array":
            if not isinstance(val, list):
                fail(key, "is not an array", schema_path)
        else:
            fail(key, "unsupported schema type: %s" % t, schema_path)
        if "pattern" in spec and not re.search(spec["pattern"], val):
            fail(key, "does not match %s" % spec["pattern"], schema_path)
        if "minimum" in spec and val < spec["minimum"]:
            fail(key, "below minimum %s" % spec["minimum"], schema_path)
        if "maximum" in spec and val > spec["maximum"]:
            fail(key, "above maximum %s" % spec["maximum"], schema_path)
        if "maxItems" in spec and len(val) > spec["maxItems"]:
            fail(key, "has more than %s items" % spec["maxItems"], schema_path)

    print("manifest matches %s" % schema["$id"])


main()
