---
commit: b1d827a80485131c6a99bbbf94074e02a6619029
---

# Sonalign

Known-audio alignment and streaming position tracking in Rust, with C and Swift APIs.

Sonalign estimates where a noisy audio stream lies within a **known reference**.
It searches for an offset, requires fresh evidence before confirming a match, and
tracks that position across successive observations. Acquisition, ambiguity,
coasting, signal loss and reacquisition are explicit states.

**Version 0.1 consumes PCEN spectral feature frames, not raw PCM.** Audio capture,
file decoding, resampling and feature extraction belong to the caller. Sonalign is
not a song-identification service or an audio playback engine. Its reference and
query features must come from the same extractor configuration.

The library originated in Pulsefield, but has no dependency on the Pulsefield app,
Swift runtime, Apple Accelerate or a network service. It is MIT licensed.

## Rust

```sh
cargo add sonalign
```

```rust
use sonalign::{Configuration, Engine, FeatureFrame, FeatureWindow, InputError, Snapshot};

fn align(
    reference: &[FeatureFrame],
    observations: Vec<Vec<FeatureFrame>>,
) -> Result<Vec<Snapshot>, InputError> {
    let hop_ms = 512.0 / 48_000.0 * 1000.0;
    let mut engine = Engine::new(reference, hop_ms, Configuration::default());
    observations.into_iter().map(|frames| {
        let window = FeatureWindow::new(frames)?;
        Ok(engine.process(&window, window.endpoint_recorded_time_ms()))
    }).collect()
}
```

`FeatureFrame` contains `recorded_time_ms: f64`, `energy_dbfs: f64` and
`pcen_mel: Vec<f32>`. `FeatureWindow::new` validates its nonempty, finite feature
data. Silence may use negative-infinite energy. Invalid configuration or nonfinite
`elapsed_ms` are documented Rust API precondition panics. An unusable reference
index produces `Failed / IndexUnavailable` snapshots.

## Swift

Add `https://github.com/Pulsefield/sonalign` as a Swift package and select the
`Sonalign` library product. Tagged releases pin an XCFramework URL and SHA-256
checksum in `Package.swift`.

```swift
import Sonalign

let aligner = try Aligner(reference: referenceFrames, hopMS: 512.0 / 48_000.0 * 1_000)
let snapshot = try aligner.process(query: queryFrames, elapsedMS: elapsedMS)
if let estimate = snapshot.estimate {
    print(estimate.referenceTimeMS, estimate.offsetMS)
}
```

The wrapper owns the Rust handle and copies feature batches while their storage is
valid. Calls to one `Aligner` must be serialized on a worker. Its lifetime does not
depend on the caller retaining previous reference or query arrays.

The Apple package contains iOS ARM64 device, ARM64/x86_64 simulator and universal
ARM64/x86_64 macOS slices. Source builds and Swift integration tests use the local
override instead of downloading a release:

```sh
python3 scripts/build.py --host --apple
SONALIGN_XCFRAMEWORK_PATH="$PWD/.build/dist/SonalignFFI.xcframework" swift test
python3 scripts/test_swift.py
```

## C and other native applications

See [include/sonalign.h](include/sonalign.h) and the
[compiled C example](tests/ffi_smoke.c). The ABI exposes `sonalign_create`,
`sonalign_process`, `sonalign_destroy` and `sonalign_abi_version`.

Input batches contain contiguous timestamps, optional energies and row-major PCEN
values. Buffers are borrowed only during each call. Rust owns the engine until it
is destroyed; callers must not access a handle concurrently or after destruction.
Check returned status codes before inspecting results. Unset numeric optionals
use NaN as described in the header. An internal unwinding panic poisons the handle.

The macOS shared library uses `@rpath/libsonalign.dylib`; an application must embed
it and supply an appropriate runtime search path (for example,
`@executable_path/lib`). Binary ZIPs include the MIT license and dependency notices.

## Signal and timing contract

- Reference and query must share the same PCEN parameters, band order, sample-rate
  normalization and hop. The validated extractor uses 24 bands, 48 kHz audio,
  1024-sample analysis windows and 512-sample hops. Sonalign does not implement that
  extractor or normalize incompatible input.
- Feature timestamps denote the **end of each analysis window** in recorded audio
  time. They are not callback-arrival times. Reference time equals query endpoint
  time plus the reported offset.
- Supply successive rolling query windows, normally about five seconds long. The
  engine checks adjacent and cumulative timestamp-grid errors against the hop.
  Preserve gaps when samples are dropped; do not silently compress the timeline.
- `elapsed_ms` records acquisition milestones. Matching, evidence freshness and
  coasting use recorded audio time instead.
- Processing allocates and may search the whole reference. Run it on a worker,
  not a real-time audio callback. Confidence is an alignment score, not a calibrated
  probability.

The matcher correlates temporally centered log-PCEN bands with variance floors.
Coarse search keeps independent alternatives, fine search refines their offsets,
and recent evidence controls confirmation and tracking. Repeated passages can be
ambiguous even with clean audio.

## Validation and limits

Rust tests cover noisy acquisition, unrelated input, repeated passages, repeated
windows, callback cadence, seek recovery, silence, discontinuities, timestamp skew,
short tracking windows and malformed inputs. A real C consumer checks the ABI;
Swift tests exercise the compiled Rust library and a synthetic two-second offset.

The source was extracted from Pulsefield's Swift spectral engine baseline
`82381d34e8166050531919b070131117bb1e13ee`. A local comparison uses identical cached
PCEN windows for both engines. The full corpus contains 24 recordings, 11 later
entries, 384 wrong-reference trials, a 100 ms cadence replay and 20 synthetic
regression scenarios. Private recordings and their extracted features are not
distributed with this repository. The public result receipt is in
[validation/swift-parity.json](validation/swift-parity.json).

Across 440 cases and 41,702 calls, published estimates and state transitions matched.
Both engines acquired all 24 recordings and all 11 later entries; neither acquired
any of the 384 wrong-reference trials. Maximum published offset difference was
0.000014 ms. Five nonleading diagnostic alternatives differed because of equal-score
ordering or identical repeated material; exact diagnostic-list parity did not pass.

This is a migration-parity check, not a claim of absolute acoustic timing accuracy.
Recorded-feature replays use a fixed energy because the exports omit energy;
synthetic cases test energy gates. Later-entry cases reset matching/tracking on
cached features, not feature-extractor state.

The current tracker estimates offset and preserves the original engine's behavior:
an accepted position can linger after the source changes, and there is no explicit
clock-rate estimator. Portable Rust kernels have not shown a speedup over optimized
Swift/Accelerate on the tested Mac. Device CPU, battery use and end-to-end capture
latency require application-level measurement.

## Building and releasing

Rust 1.88 or newer is required. From a clean checkout:

```sh
cargo test --locked --release
cargo clippy --locked --all-targets -- -D warnings
cargo publish --dry-run
```

`scripts/build.py` uses Cargo from `PATH`. `--host` produces the native library,
C headers and `sonalign-replay` CLI. `--apple` requires Xcode and produces a SwiftPM
XCFramework ZIP; `--android` and `--windows` produce cross-compiled static archives.
The replay CLI consumes feature-array files and explicit query slices; it does not
decode audio files. Run `sonalign-replay --help` for its entry point.

CI tests native Rust on Linux, macOS and Windows, checks the minimum Rust version,
builds the Apple package and runs Swift/C integration checks. iOS example linking
checks SDK compatibility; it is not device execution. Android archives still need
an application's NDK/JNI link and 16 KB page-size validation.

To release, update `Cargo.toml` and `Cargo.lock` on `main`, then run the **Release**
workflow from GitHub Actions with the version (without `v`). The workflow:

1. Checks the requested version and runs the test/build jobs.
2. Computes the Apple ZIP checksum and commits the corresponding Swift manifest.
3. Tags that commit, creates the GitHub release and uploads immutable artifacts.
4. Publishes the Cargo package if that version is not already on crates.io.

The workflow uses the `release` environment, restricted to `main`. Repository
variable `CARGO_TRUSTED_PUBLISHING=true` selects crates.io OIDC authentication;
the `CARGO_REGISTRY_TOKEN` environment secret is the fallback. Release tags and
existing assets are checked before writes and are never force-replaced. A retry
accepts only the original source or its exact generated Swift-checksum commit;
an already-published crate must match the tagged source and clean VCS provenance. Concurrent
release runs are serialized, and an intervening `main` update prevents the release
commit from being pushed.
