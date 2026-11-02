# Rewrites kitty-isms baked into its compiled extension with same-length,
# NUL-padded replacements (byte-for-byte, so the Mach-O layout is untouched).
import sys
path = sys.argv[1]
data = bytearray(open(path, 'rb').read())
for old, new in [(b"New OS Window\0", b"New Window\0")]:
    assert len(new) <= len(old)
    new = new.ljust(len(old), b"\0")
    n = data.count(old)
    data = bytearray(data.replace(old, new))
    print(f"{old!r} -> {new!r}: {n} occurrence(s)")
open(path, 'wb').write(data)
