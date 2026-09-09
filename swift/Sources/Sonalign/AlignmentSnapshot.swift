import SonalignFFI

public enum AlignmentState: UInt32, Sendable {
    case ready = 0, listening, locking, confirmed, locked, drifting, relocking, lost, failed
}

public enum LockPhase: UInt32, Sendable {
    case none = 0, provisional, confirmed, final
}

public enum AlignmentStage: UInt32, Sendable {
    case readiness = 0, landmarkCoarse, fastTimingVerify, robustVerify, tracking, relock
}

public enum WithholdReason: UInt32, Sendable {
    case insufficientDuration = 1
    case insufficientEnergy, insufficientActiveFrames, insufficientLandmarkEvidence
    case weakAlignmentPeak, ambiguousOffset, unstableTrackingResidual, lostSignal
    case decodeFailed, indexUnavailable
}

public struct AlignmentEstimate: Equatable, Sendable {
    public let queryEndpointRecordedTimeMS: Double
    public let referenceTimeMS: Double
    /// referenceTimeMS - queryEndpointRecordedTimeMS; a positive value means
    /// the captured content is farther into the reference than its recorded clock.
    public let offsetMS: Double
}

public struct AlignmentDiagnostics: Equatable, Sendable {
    public let globalSearch: Bool
    public let correlation: Double
    /// Available only for a global search.
    public let competingPeakMargin: Double?
    public let freshEvidenceMS: Double
    public let coastMS: Double
}

public struct AlignmentSnapshot: Equatable, Sendable {
    public let state: AlignmentState
    public let phase: LockPhase
    public let stage: AlignmentStage
    public let withholdReason: WithholdReason?
    public let queryEndpointRecordedTimeMS: Double
    public let estimate: AlignmentEstimate?
    /// An alignment score, not a calibrated probability.
    public let confidence: Double
    public let firstProvisionalLockElapsedMS: Double?
    public let confirmedLockElapsedMS: Double?
    public let finalLockElapsedMS: Double?
    public let diagnostics: AlignmentDiagnostics

    internal init(_ result: SonalignResultV1) throws {
        guard let state = AlignmentState(rawValue: result.state),
              let phase = LockPhase(rawValue: result.phase),
              let stage = AlignmentStage(rawValue: result.stage),
              result.withhold_reason == 0 || WithholdReason(rawValue: result.withhold_reason) != nil,
              result.has_estimate <= 1, result.global_search <= 1 else {
            throw SonalignError.invalidResult
        }
        self.state = state
        self.phase = phase
        self.stage = stage
        self.withholdReason = WithholdReason(rawValue: result.withhold_reason)
        self.queryEndpointRecordedTimeMS = result.query_endpoint_recorded_time_ms
        if result.has_estimate == 1 {
            guard result.query_endpoint_recorded_time_ms.isFinite,
                  result.reference_time_ms.isFinite, result.offset_ms.isFinite else {
                throw SonalignError.invalidResult
            }
            self.estimate = AlignmentEstimate(
                queryEndpointRecordedTimeMS: result.query_endpoint_recorded_time_ms,
                referenceTimeMS: result.reference_time_ms,
                offsetMS: result.offset_ms
            )
        } else {
            self.estimate = nil
        }
        self.confidence = result.confidence
        self.firstProvisionalLockElapsedMS = Self.optional(result.first_provisional_lock_elapsed_ms)
        self.confirmedLockElapsedMS = Self.optional(result.confirmed_lock_elapsed_ms)
        self.finalLockElapsedMS = Self.optional(result.final_lock_elapsed_ms)
        self.diagnostics = AlignmentDiagnostics(
            globalSearch: result.global_search != 0,
            correlation: result.correlation,
            competingPeakMargin: Self.optional(result.competing_peak_margin),
            freshEvidenceMS: result.fresh_evidence_ms,
            coastMS: result.coast_ms
        )
    }

    private static func optional(_ value: Double) -> Double? {
        value.isNaN ? nil : value
    }
}
