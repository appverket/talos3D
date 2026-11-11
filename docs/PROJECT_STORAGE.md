# Native project storage

Native saves and recovery files use a lossless compact container. The authored
project schema is still version 3; container versioning is independent of schema
versioning. Existing object-form JSON files remain readable through the same
loader, including existing schema migration and producer compatibility checks.

## Container version 1

The UTF-8 JSON envelope is a five-item tuple:

```json
["TALOS3D",1,"msgpack-gzip-buffers",186316002,"<base64 payload>"]
```

The fourth field is the uncompressed payload length. The final field is standard
Base64 containing one gzip member (fast compression), whose body is MessagePack
with named map fields:

- `root`: the complete authored document, with interned arrays replaced by null.
- `buffers`: unique numeric arrays.
- `references`: `{pointer, buffer}` records using escaped JSON Pointers and
  zero-based buffer indices.

Arrays with at least 64 items qualify when each item is a number or a row of
1–16 numbers. Exact MessagePack bytes establish identity: BLAKE3 locates candidate
matches, followed by byte comparison. Numbers are neither quantized nor rounded;
integer/float distinctions and signed zero survive. Object keys are sorted for
deterministic encoding. Unknown plugin data is included in the generic document.

Decoding reconstructs independent arrays. Sharing is a storage optimization; it
does not merge entities, change Definition/Occurrence relationships, or couple
subsequent edits. There is no geometry inference, cache eviction, or removal of
hidden configurations or unreferenced library assets.

The decoder checks container version, codec, Base64, gzip checksum and exact
length, trailing compressed/MessagePack bytes, buffer indices, numeric shape,
reference locations, and attempts to overwrite non-null content. Both packed
payload and conservative reference expansion are bounded at 8 GiB (and the
platform address-space limit). This is a serialized-size ceiling, not a RAM
budget: generic JSON values and restored geometry can use substantially more
memory. Parsing and container validation finish before world restoration starts.

## API and compatibility

`capture_project_file_bytes` produces the native container. Save, recovery,
project import, linked-project readers and library installation accept either
representation through `project_codec::decode`.

`capture_project_bytes` deliberately retains the object-form JSON snapshot API
used by browser shells, project-store clients, content hashes and delayed-save
acknowledgements. It now omits pretty-print whitespace. Linked subset exports
also retain object JSON. Existing consumers can continue inspecting snapshot
fields without unpacking a native container.

Older Talos3D binaries and browser file pickers that validate raw JSON objects
cannot open the new container directly. Use the explicit lossless legacy export
below when transferring a new save to those clients. Updating the native reader
does not imply that an independently deployed web shell has been updated.

```sh
cargo run --release -p talos3d-core --example project_storage -- compact OLD NEW
cargo run --release -p talos3d-core --example project_storage -- legacy NEW LEGACY
cargo run --release -p talos3d-core --example project_storage -- verify OLD NEW
```

Conversion compares complete canonical document digests before writing and
refuses to overwrite any existing destination, including the input. Verification
includes opaque extension values and all numeric data, not just entity counts.

Native disk saves write and sync a unique sibling temporary file, then rename
it over the destination and sync its parent directory on Unix. A failed write or
rename never deletes the previous destination. Existing file permissions are
retained. Recovery uses the same durable-write implementation.

## Large-model verification

A private 5,187-entity fixture decreased from 1,720,888,326 bytes of pretty JSON
to 58,245,453 bytes (96.6% smaller). Whitespace removal alone produced about
633 MB. Compact encoding found 2,599 qualifying array references and 1,258 unique
buffers; the uncompressed MessagePack payload was 186,316,002 bytes. No fixture
data is included in this public repository.

The fixture contains 445 Definitions, all reachable through direct occurrences
or nested Definition references, 343 direct Occurrences and 2,144 standalone
triangle meshes. Definition reuse already exists; storage interning additionally
shares repeated numeric buffers across independently authored objects.

Codec regressions cover exact numbers, unknown fields, determinism, independent
decoded values, malformed references and corrupt/truncated input. Persistence
tests cover native and legacy snapshot equivalence, opaque entity preservation,
failure before world mutation and durable replacement. The full application is
also checked by opening both representations, comparing authored identities and
bounds, saving/reopening and visually inspecting the rendered model.
