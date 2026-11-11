"""Fetch the exact geometry used by xLog from its upstream publisher."""
import hashlib
from pathlib import Path
import urllib.request

URL = "https://raw.githubusercontent.com/amazingproducer/dxcc-world-map/f15968402e21faa192ebe35bd91cdfcc4f0fa8db/dxcc.geojson"
SHA256 = "e9b086eff7fb690e9990e10855f3fb494f9d1115e4af1928f6a8b545efef4568"
TARGET = Path(__file__).resolve().parents[1] / "Sources/XLogCore/Resources/dxcc-boundaries.geojson"


def main():
    if TARGET.exists():
        data = TARGET.read_bytes()
    else:
        with urllib.request.urlopen(URL, timeout=60) as response:
            data = response.read()
    if hashlib.sha256(data).hexdigest() != SHA256:
        raise SystemExit("Geometry checksum mismatch. Verify the upstream file before building.")
    if not TARGET.exists():
        with TARGET.open("xb") as destination:
            destination.write(data)
    print("Verified DXCC geometry", SHA256)


if __name__ == "__main__":
    main()
