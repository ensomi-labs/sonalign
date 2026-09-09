import SonalignFFI

public enum SonalignError: Error, Equatable, Sendable {
    case invalidInput(String)
    case incompatibleABIVersion(UInt32)
    case invalidArgument
    /// The native engine is poisoned after a panic. Release this aligner and create another.
    case nativePanic
    case poisoned
    case nativeFailure(Int32)
    case invalidResult
}

/// Aligns rolling query features against one reference and tracks its position.
///
/// Own this instance on a serial worker or actor and serialize construction,
/// processing, and destruction. It is intentionally not Sendable. These calls
/// allocate and search; never make them on an audio callback or the main thread.
/// No PCM, decoding, resampling, or feature extraction is performed here.
public final class Aligner {
    private let handle: OpaquePointer
    private let bandCount: Int

    /// Copies reference features into the native engine.
    /// - Parameter hopMS: The analysis hop shared by reference and query,
    ///   in milliseconds (for example, 512.0 / 48_000.0 * 1_000.0).
    public init(reference: [FeatureFrame], hopMS: Double) throws {
        let version = sonalign_abi_version()
        guard version == 1 else { throw SonalignError.incompatibleABIVersion(version) }
        guard hopMS.isFinite, hopMS > 0 else {
            throw SonalignError.invalidInput("hopMS must be finite and positive")
        }
        var created: OpaquePointer?
        try Self.withBatch(reference) { batch in
            try Self.check(sonalign_create(batch, hopMS, &created))
        }
        guard let created else { throw SonalignError.invalidResult }
        self.handle = created
        self.bandCount = reference[0].pcenMel.count
    }

    deinit {
        sonalign_destroy(handle)
    }

    /// Processes a nonempty rolling query window, ordered by recorded audio time.
    /// Windows may overlap; repeated frames do not count as fresh evidence.
    /// Start with at least 2.5 seconds and retain 5 seconds for acquisition.
    /// A 2-second window is sufficient once locked.
    /// - Parameter elapsedMS: Elapsed session time in milliseconds, used for lock milestones.
    /// - Returns: The engine's current state; withholding and lost signals are results,
    ///   while malformed input and native failures throw.
    public func process(query: [FeatureFrame], elapsedMS: Double) throws -> AlignmentSnapshot {
        guard elapsedMS.isFinite else {
            throw SonalignError.invalidInput("elapsedMS must be finite")
        }
        guard query.first?.pcenMel.count == bandCount else {
            throw SonalignError.invalidInput("query bands must match the reference")
        }
        var result = SonalignResultV1()
        try Self.withBatch(query) { batch in
            try Self.check(sonalign_process(handle, batch, elapsedMS, &result))
        }
        return try AlignmentSnapshot(result)
    }

    private static func check(_ status: Int32) throws {
        switch status {
        case Int32(SONALIGN_OK): return
        case Int32(SONALIGN_INVALID_ARGUMENT): throw SonalignError.invalidArgument
        case Int32(SONALIGN_PANIC): throw SonalignError.nativePanic
        case Int32(SONALIGN_POISONED): throw SonalignError.poisoned
        default: throw SonalignError.nativeFailure(status)
        }
    }

    private static func withBatch<T>(
        _ frames: [FeatureFrame],
        _ body: (SonalignFeatureBatch) throws -> T
    ) throws -> T {
        guard let first = frames.first, !first.pcenMel.isEmpty else {
            throw SonalignError.invalidInput("features and PCEN bands must be nonempty")
        }
        let bands = first.pcenMel.count
        let (elements, overflow) = frames.count.multipliedReportingOverflow(by: bands)
        guard !overflow else { throw SonalignError.invalidInput("feature batch is too large") }
        var times = [Double]()
        var energies = [Double]()
        var pcen = [Float]()
        times.reserveCapacity(frames.count)
        energies.reserveCapacity(frames.count)
        pcen.reserveCapacity(elements)
        for frame in frames {
            guard frame.pcenMel.count == bands,
                  frame.recordedTimeMS.isFinite,
                  !frame.energyDBFS.isNaN, frame.energyDBFS != .infinity,
                  frame.pcenMel.allSatisfy({ $0.isFinite && $0 >= 0 }) else {
                throw SonalignError.invalidInput("features require finite timestamps, valid energies, and matching finite nonnegative PCEN bands")
            }
            times.append(frame.recordedTimeMS)
            energies.append(frame.energyDBFS)
            pcen.append(contentsOf: frame.pcenMel)
        }
        // The three contiguous arrays remain borrowed for the entire native call.
        // Rust copies the input; no pointer escapes any withUnsafeBufferPointer closure.
        return try times.withUnsafeBufferPointer { times in
            try energies.withUnsafeBufferPointer { energies in
                try pcen.withUnsafeBufferPointer { pcen in
                    try body(SonalignFeatureBatch(
                        recorded_times_ms: times.baseAddress,
                        energies_dbfs: energies.baseAddress,
                        pcen: pcen.baseAddress,
                        frame_count: frames.count,
                        band_count: bands
                    ))
                }
            }
        }
    }
}
