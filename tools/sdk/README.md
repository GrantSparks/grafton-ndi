# Local SDK review tools

These tools inspect authorized local SDKs. They never download an SDK, load a native
SDK library, initialize NDI, or execute a cross-target program. Use Python 3.11+,
Clang, and the appropriate target toolchain. ELF inspection uses `readelf`; PE,
Mach-O, archive and object-probe inspection use LLVM tools. Versioned LLVM command
names are discovered when unversioned names are absent.

SDK snapshots contain vendor declarations. The snapshot command requires an output
outside this repository. Keep snapshots, SDK archives, generated Advanced bindings
and private acquisition URLs in private storage. The tracked registry contains
reviewed identifiers and fingerprints, not whole vendor ABI definitions. Header
notices and redistribution provenance still need review before publication.

The current retention and validation scope comprises 12 complete platform bundles:
Standard 6.3.2 for Linux, Windows, Apple and Android, plus Standard and Advanced
6.4.0.2 for those four platforms. The [private SDK store](../../ci/sdk-store.md)
preserves the reviewed originals. Earlier-release collection is not part of this
work or a prerequisite for its validation. This scope does not establish a new
minimum SDK version or prove compatibility with releases that were not exercised.

For each future SDK release, retain its complete original platform bundles, record
their exact identities in the shared catalog, and review them against the retained
baseline before changing the validation policy. Retention is prospective; there
is no task to reconstruct every historical SDK release.

## Snapshot, compare and classify

```sh
python tools/sdk/audit.py snapshot \
  --sdk-root "$NDI_SDK_DIR" --edition standard \
  --target x86_64-unknown-linux-gnu \
  --include-header Processing.NDI.Lib.File.h \
  --output /path/to/private/sdk-current.json

python tools/sdk/audit.py diff \
  --old /path/to/private/sdk-baseline.json \
  --new /path/to/private/sdk-current.json \
  --output /path/to/private/sdk-diff.json

python tools/sdk/audit.py classify \
  --manifest /path/to/private/sdk-current.json \
  --registry api/ndi-api.toml \
  --output /path/to/private/sdk-review.json
```

For Advanced, select `--edition advanced` and explicitly include
`Processing.NDI.Advanced.h`. Only include File headers on an SDK containing them.
Use equal include configurations when comparing versions. `--archive` records an
original local archive's exact SHA256/size. `--profile` cross-checks target/edition
against `build_profiles.tsv`; it is an artifact-selection assertion, not proof of
CPU/device behavior. `--clang-arg=...` passes explicit target sysroot/defines.

Snapshot schema 2 records root-relative header hashes, version files, target and
edition, compiler executable hash/version, transitive header identities,
include-affecting environment, semantic declarations, enum values, macros, named
record layouts and exact binary identities. Its content fingerprint detects later
modification, not malicious forgery. `edition` is caller-declared: verify archive
identity and actual module artifacts through the acquisition/profile validation.

Comparison rejects changed compiler/target/include configuration, changed shared
external headers, and ambiguous directory/SONAME correlations. An intentional
`--allow-configuration-change` emits an explicitly incomparable report; it is not a
compatibility pass. Header/doc-contract changes remain review items even if API
fingerprints are identical. New, changed or unknown declarations, enum values,
macros and headers fail classification until explicitly reviewed. There is no
production auto-accept command.

`earliest_observed` means the first inspected artifact, not an introduction-version
claim. `disposition`, `wrapper`, `rationale` and `evidence` are separate from ABI
fingerprints. `planned`/`vendor_blocked` entries remain visible. Add
`--require-implemented` for the agreed implementation-coverage gate. Individually
approved `reviewed_unavailable` declarations require a `scope_decision` and appear
in `approved_unavailable`; they are never reported implemented. These currently
include the two File recorder pointer getters whose lifetime contracts remain
unverified. `complete_sdk_surface` stays false while any such item exists;
`--require-complete-surface` rejects these restrictions as well. A passed coverage gate
still does not establish runtime, licensing, hardware or interoperability support.
Use `--baseline` to reject removal of retained same-target declarations. Undocumented
native exports need their own explicit `binary_export` classification.
The initial accepted observations cover the recorded Linux C11 configurations;
other target/preprocessor observations require deliberate review, not a wildcard.

## Capability metadata

```sh
python tools/sdk/audit.py emit-capabilities
python tools/sdk/audit.py emit-capabilities --check
```

The reviewed registry generates `api/capabilities.rs`: small typed tuples containing
module, required symbol names and documented runtime minima. It contains no native
ABI signatures or ownership implementation. Runtime dispatch must apply complete
lifecycle groups to one pinned module, separately validate compiled ABI availability,
and treat enum-only version gates conservatively. Symbol presence alone is not
operational capability. Do not replace handwritten safe wrappers with generated
calls. CI should run `--check` to prevent registry/generated-metadata drift.

## Independent target ABI comparison

```sh
python tools/sdk/abi_probe.py \
  --sdk-root "$NDI_SDK_DIR" \
  --rust-bindings /path/to/the/actual/build/out/ndi_lib.rs \
  --target x86_64-unknown-linux-gnu \
  --cc cc --output /path/to/private/abi-result.json
```

`api/abi-probes.toml` selects C/Rust type, field and constant mappings. The command
compiles independent C and Rust constant arrays into object sections, checks each
object's architecture/format against the requested target, then compares bytes.
It does not execute target code or link NDI. Size, alignment, nested union offsets,
field offsets and enum values can be checked even when target execution is absent.
Pass the actual target compiler and `--cc-arg`/`--rustc-arg` sysroot/target options
for cross builds. A host compiler silently producing a host object is rejected.
Rust target standard libraries and a standalone ABI module are required.

The common spec currently probes 128 values across 12 records and common enums.
`discovery_probe.py` extracts the actual private discovery records and compares
44 target values. `file_probe.py` extracts all 11 actual private File records and
compares 169 target values, including every field offset and field size. The latter
detects array-extent changes hidden by unchanged structure size and padding.
Both accept `--sdk-root`, `--target`, `--output`, `--cc`, `--cc-arg`, `--rustc`,
`--rustc-arg` and optionally `--repo`. Syntax/mapping changes fail explicitly.
File probes require the SDK File headers. Missing target headers or Rust target
libraries are failures, not successful host substitutions. A 64-bit result does
not certify 32-bit targets. Evidence records the actual production source hashes,
compiler identities, commands and transitive C header hashes.
For additional private Advanced records, add independently reviewed probes and
point `--rust-bindings` at the implementation's actual declarations. Testing a
second generated copy does not validate a handwritten ABI module used elsewhere.
This probe does not yet establish every function signature/callback calling
convention. The C target compiler must support the used section attribute (Clang
or GCC); a Windows MSVC environment can use Clang with that target's headers.

Prefer an independent C frontend (GCC/MSVC where applicable) relative to bindgen's
Clang. Actual runtime/callback/media tests, direct-import audits, and native target
execution remain separate gates. Select a pinned installed Rust toolchain, e.g.
with `RUSTUP_TOOLCHAIN`, to avoid a rustup update during an offline audit.

## Tests and limitations

```sh
python -m unittest discover -s tools/sdk -p 'test_*.py' -v
```

Tests use small independently authored C/Rust fixtures. They mutate functions,
enums and equal-size field order, verify comment changes do not fabricate ABI
changes, check exact snapshot reproducibility and review gates, reject differing
configurations/tampering, reject duplicate SONAME identities, and demonstrate that
misordered Rust fields and padding-hidden array extents fail against the independent
C object probe. Reviewed unavailable APIs never imply complete SDK coverage. Tests
require no proprietary SDK or network. Missing compiler tools are explicit test skips.

The extractor is a focused review aid, not a complete C/C++ or binary-ABI verifier.
It parses the selected C11 umbrella/extra headers; C++ inline behavior and some
Clang attribute payloads need source review. Named record layouts and recursive
anonymous fields are retained; source-only changes remain covered by header hashes.
Source-location fallback requires a unique token/offset match and fails ambiguity.
Mach-O symbols are recorded per slice as *defined external globals*, not falsely
labelled export-trie/reexport proof. Archive definitions/import-library entries are
separate from ELF/PE exports. Runtime-loaded codecs, CPU instructions, dependency
closure, allocator/free contracts and operational support are not inferred from
symbol names or version strings. Unsupported binary inspection is explicit.

An SDK upgrade is complete only after every changed item has a reviewed disposition
and every check in the approved required scope has passed. Required CI samples
runtime and compatibility on major platform families while retaining ABI/link or
package coverage for the other supported profiles. Use the optional extended
audit to investigate broader SDK/compiler combinations. Physical-device
qualification is recorded separately. Public uploads and releases remain separate
user-authorized actions.

The hosted matrix, mobile harnesses and development-store integration
must be assessed against their actual execution evidence. Local inspection,
compiler probes and fixture tests do not establish hosted, devcontainer, simulator
or physical-device results. See [the validation record](../../docs/sdk-validation.md)
and [CI policy](../../ci/README.md) for the current distinction.
