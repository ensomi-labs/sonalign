import Foundation
import XCTest
import Sonalign

final class AlignerTests: XCTestCase {
    func testAcquiresOffsetTracksShortWindowAndLosesSilence() throws {
        var reference = makeFrames(count: 1_500)
        let query = (0..<500).map { index in
            FeatureFrame(recordedTimeMS: Double(index) * 20, energyDBFS: -20, pcenMel: reference[index + 100].pcenMel)
        }
        let aligner = try Aligner(reference: reference, hopMS: 20)
        // The C ABI must have copied the reference, including flattened PCEN storage.
        reference.removeAll()

        let short = try aligner.process(query: Array(query[0...50]), elapsedMS: 1_000)
        XCTAssertEqual(short.state, .listening)
        XCTAssertEqual(short.withholdReason, .insufficientDuration)
        XCTAssertNil(short.estimate)
        XCTAssertNil(short.firstProvisionalLockElapsedMS)

        var acquired: AlignmentSnapshot?
        for end in stride(from: 125, through: 350, by: 25) {
            acquired = try aligner.process(query: Array(query[max(0, end - 250)...end]), elapsedMS: Double(end) * 20)
        }
        let locked = try XCTUnwrap(acquired)
        XCTAssertEqual(locked.state, .locked)
        XCTAssertEqual(locked.phase, .final)
        XCTAssertNil(locked.withholdReason)
        XCTAssertNotNil(locked.firstProvisionalLockElapsedMS)
        XCTAssertNotNil(locked.confirmedLockElapsedMS)
        XCTAssertNotNil(locked.finalLockElapsedMS)
        let estimate = try XCTUnwrap(locked.estimate)
        XCTAssertEqual(estimate.offsetMS, 2_000, accuracy: 8)
        XCTAssertEqual(estimate.queryEndpointRecordedTimeMS, 7_000)
        XCTAssertEqual(estimate.referenceTimeMS, estimate.queryEndpointRecordedTimeMS + estimate.offsetMS, accuracy: 0.001)

        let tracked = try aligner.process(query: Array(query[300...400]), elapsedMS: 8_000)
        XCTAssertEqual(tracked.state, .locked)
        XCTAssertEqual(tracked.stage, .tracking)
        XCTAssertEqual(try XCTUnwrap(tracked.estimate).offsetMS, 2_000, accuracy: 8)

        let silence = (0..<250).map { index in
            FeatureFrame(recordedTimeMS: 10_000 + Double(index) * 20, energyDBFS: -.infinity, pcenMel: [Float](repeating: 0, count: 24))
        }
        let lost = try aligner.process(query: silence, elapsedMS: 15_000)
        XCTAssertEqual(lost.state, .lost)
        XCTAssertEqual(lost.withholdReason, .insufficientEnergy)
        XCTAssertNil(lost.estimate)
    }

    func testInvalidInputsThrowAndLeaveEngineUsable() throws {
        let reference = makeFrames(count: 1_500)
        XCTAssertThrowsError(try Aligner(reference: [], hopMS: 20))
        for hop in [0, -1, .nan, .infinity] as [Double] {
            XCTAssertThrowsError(try Aligner(reference: reference, hopMS: hop))
        }
        let aligner = try Aligner(reference: reference, hopMS: 20)
        let invalid = [
            FeatureFrame(recordedTimeMS: 0, energyDBFS: -20, pcenMel: []),
            FeatureFrame(recordedTimeMS: 0, energyDBFS: -20, pcenMel: [1]),
            FeatureFrame(recordedTimeMS: .nan, energyDBFS: -20, pcenMel: reference[0].pcenMel),
            FeatureFrame(recordedTimeMS: 0, energyDBFS: .nan, pcenMel: reference[0].pcenMel),
            FeatureFrame(recordedTimeMS: 0, energyDBFS: .infinity, pcenMel: reference[0].pcenMel),
            FeatureFrame(recordedTimeMS: 0, energyDBFS: -20, pcenMel: [Float](repeating: .nan, count: 24)),
            FeatureFrame(recordedTimeMS: 0, energyDBFS: -20, pcenMel: [Float](repeating: .infinity, count: 24)),
            FeatureFrame(recordedTimeMS: 0, energyDBFS: -20, pcenMel: [Float](repeating: -1, count: 24)),
        ]
        XCTAssertThrowsError(try aligner.process(query: [], elapsedMS: 0))
        for frame in invalid {
            XCTAssertThrowsError(try aligner.process(query: [frame], elapsedMS: 0)) { error in
                guard case SonalignError.invalidInput = error else {
                    return XCTFail("Unexpected error: \(error)")
                }
            }
        }
        XCTAssertThrowsError(try aligner.process(query: [reference[0], invalid[1]], elapsedMS: 0))
        XCTAssertThrowsError(try aligner.process(query: [reference[0]], elapsedMS: .nan))
        let snapshot = try aligner.process(query: Array(reference[0...50]), elapsedMS: 1_000)
        XCTAssertEqual(snapshot.state, .listening)
        XCTAssertEqual(snapshot.withholdReason, .insufficientDuration)
    }

    func testIrregularReferenceMapsNativeFailureToTypedResult() throws {
        let regular = makeFrames(count: 300)
        let irregular = regular.enumerated().map { index, frame in
            FeatureFrame(recordedTimeMS: frame.recordedTimeMS + (index == 100 ? 10 : 0), energyDBFS: frame.energyDBFS, pcenMel: frame.pcenMel)
        }
        let aligner = try Aligner(reference: irregular, hopMS: 20)
        let snapshot = try aligner.process(query: Array(regular[0...250]), elapsedMS: 5_000)
        XCTAssertEqual(snapshot.state, .failed)
        XCTAssertEqual(snapshot.withholdReason, .indexUnavailable)
        XCTAssertNil(snapshot.estimate)
    }

    private func makeFrames(count: Int) -> [FeatureFrame] {
        (0..<count).map { index in
            let t = Double(index)
            let pcen = (0..<24).map { band -> Float in
                let a = Double((band + 3) * 8)
                return Float(exp(
                    0.5 * sin(t * (0.061 + a * 0.00037) + a)
                    + 0.5 * sin(t * (0.113 + a * 0.00019) + t * t * 0.00013)
                ))
            }
            return FeatureFrame(recordedTimeMS: t * 20, energyDBFS: -20, pcenMel: pcen)
        }
    }
}
