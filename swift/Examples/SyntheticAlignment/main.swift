import Foundation
import Sonalign

// A deterministic feature-only example. In an app, perform this on a serial worker.
let reference = (0..<1_500).map { index -> FeatureFrame in
    let t = Double(index)
    let bands = (0..<24).map { band -> Float in
        let a = Double((band + 3) * 8)
        return Float(exp(
            0.5 * sin(t * (0.061 + a * 0.00037) + a)
            + 0.5 * sin(t * (0.113 + a * 0.00019) + t * t * 0.00013)
        ))
    }
    return FeatureFrame(recordedTimeMS: t * 20, energyDBFS: -20, pcenMel: bands)
}
let query = (0..<500).map { index in
    FeatureFrame(recordedTimeMS: Double(index) * 20, energyDBFS: -20, pcenMel: reference[index + 100].pcenMel)
}
let aligner = try Aligner(reference: reference, hopMS: 20)
for end in stride(from: 125, through: 450, by: 25) {
    let window = Array(query[max(0, end - 250)...end])
    let snapshot = try aligner.process(query: window, elapsedMS: Double(end) * 20)
    let offset = snapshot.estimate.map { String(format: "%.1f ms", $0.offsetMS) } ?? "withheld"
    print("\(Double(end) * 20) ms: \(snapshot.state), offset \(offset)")
}
