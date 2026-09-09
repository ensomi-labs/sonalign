/// One PCEN spectral frame. Both streams must use the same feature extraction settings.
public struct FeatureFrame: Equatable, Sendable {
    /// Analysis-window end in recorded audio time, in milliseconds.
    /// This is not callback arrival time or wall-clock time.
    public let recordedTimeMS: Double
    /// Actual frame energy in dBFS. Negative infinity is accepted for silence.
    public let energyDBFS: Double
    /// A nonempty array of finite, nonnegative PCEN mel-band values.
    public let pcenMel: [Float]

    public init(recordedTimeMS: Double, energyDBFS: Double, pcenMel: [Float]) {
        self.recordedTimeMS = recordedTimeMS
        self.energyDBFS = energyDBFS
        self.pcenMel = pcenMel
    }
}
