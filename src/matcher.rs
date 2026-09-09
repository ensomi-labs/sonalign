use crate::{FeatureFrame, FeatureWindow};

/// A candidate maps capture time to reference time by adding `offset_ms`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Candidate {
    pub(crate) offset_ms: f64,
    pub(crate) score: f64,
    pub(crate) first_half_score: f64,
    pub(crate) second_half_score: f64,
    pub(crate) recent_score: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MatchResult {
    pub(crate) candidates: Vec<Candidate>,
}

impl MatchResult {
    pub(crate) fn best(&self) -> Option<&Candidate> {
        self.candidates.first()
    }

    pub(crate) fn margin(&self) -> f64 {
        self.best().map_or(0.0, |best| {
            best.score - self.candidates.get(1).map_or(0.0, |next| next.score)
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Bands {
    values: Vec<Vec<f32>>,
    energy: Vec<Vec<f64>>,
}

impl Bands {
    fn new(values: Vec<Vec<f32>>) -> Self {
        let energy = values
            .iter()
            .map(|band| {
                let mut sums = Vec::with_capacity(band.len() + 1);
                sums.push(0.0);
                for &value in band {
                    let value = f64::from(value);
                    sums.push(sums.last().unwrap() + value * value);
                }
                sums
            })
            .collect();
        Self { values, energy }
    }

    fn count(&self) -> usize {
        self.values.first().map_or(0, Vec::len)
    }

    fn reduced(&self, factor: usize) -> Self {
        Self::new(
            self.values
                .iter()
                .map(|band| {
                    band.chunks_exact(factor)
                        .map(|chunk| {
                            let mut sum = 0.0_f32;
                            for &value in chunk {
                                sum += value;
                            }
                            sum / factor as f32
                        })
                        .collect()
                })
                .collect(),
        )
    }
}

/// Temporal correlation of centered log-PCEN bands, matching the Swift V2 engine.
/// Reference preprocessing is immutable and reused for every query window.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SpectralMatcher {
    fine: Bands,
    coarse: Bands,
    start_ms: f64,
    hop_ms: f64,
    radius: usize,
}

impl SpectralMatcher {
    const REDUCTION: usize = 4;
    const VARIANCE_FLOOR: f64 = 0.001;

    pub(crate) fn new(frames: &[FeatureFrame], hop_ms: f64) -> Option<Self> {
        if !is_regular(frames, hop_ms) {
            return None;
        }
        let band_count = frames.first()?.pcen_mel.len();
        if band_count == 0 {
            return None;
        }
        let radius = (250.0 / hop_ms).round().max(1.0);
        // Reject hops that cannot be represented by the centering window.
        if radius >= ((usize::MAX - 17) / 2) as f64 {
            return None;
        }
        let radius = radius as usize;
        let fine = Bands::new(centered(frames, band_count, radius)?);
        let coarse = fine.reduced(Self::REDUCTION);
        Some(Self {
            fine,
            coarse,
            start_ms: frames[0].recorded_time_ms,
            hop_ms,
            radius,
        })
    }

    pub(crate) fn accepts_timeline(&self, query: &FeatureWindow) -> bool {
        is_regular(&query.frames, self.hop_ms)
            && query
                .frames
                .iter()
                .all(|frame| frame.pcen_mel.len() == self.fine.values.len())
    }

    /// `None` searches the whole reference; tracking supplies a closed offset range.
    pub(crate) fn match_query(
        &self,
        query: &FeatureWindow,
        range_ms: Option<(f64, f64)>,
    ) -> MatchResult {
        if !self.accepts_timeline(query) || query.frames.len() <= 2 * self.radius + 16 {
            return MatchResult::default();
        }
        let Some(values) = centered(&query.frames, self.fine.values.len(), self.radius) else {
            return MatchResult::default();
        };

        // Retain only frames with complete context already in the rolling window.
        let query_bands = Bands::new(
            values
                .into_iter()
                .map(|band| band[self.radius..band.len() - self.radius].to_vec())
                .collect(),
        );
        let query_start_ms = query.frames[self.radius].recorded_time_ms;
        let Some(maximum_start) = self.fine.count().checked_sub(query_bands.count()) else {
            return MatchResult::default();
        };

        let ranges = if let Some((lower, upper)) = range_ms {
            if !lower.is_finite() || !upper.is_finite() || lower > upper {
                return MatchResult::default();
            }
            let low = ((query_start_ms + lower - self.start_ms) / self.hop_ms)
                .ceil()
                .max(0.0);
            let high = ((query_start_ms + upper - self.start_ms) / self.hop_ms)
                .floor()
                .min(maximum_start as f64);
            if low > high {
                return MatchResult::default();
            }
            vec![(low as usize, high as usize)]
        } else {
            let reduced_query = query_bands.reduced(Self::REDUCTION);
            if reduced_query.count() == 0 || self.coarse.count() < reduced_query.count() {
                return MatchResult::default();
            }
            let scores = correlations(
                &self.coarse,
                &reduced_query,
                0,
                self.coarse.count() - reduced_query.count(),
            );
            let separation = (750.0 / (self.hop_ms * Self::REDUCTION as f64)) as usize;
            peaks(&scores, separation.max(1), 8)
                .into_iter()
                .filter_map(|peak| {
                    let center = peak * Self::REDUCTION;
                    let low = center.saturating_sub(Self::REDUCTION);
                    let high = maximum_start.min(center + Self::REDUCTION);
                    (low <= high).then_some((low, high))
                })
                .collect()
        };

        let mut candidates = Vec::with_capacity(ranges.len());
        for (low, high) in ranges {
            let scores = correlations(&self.fine, &query_bands, low, high);
            // Swift's max(by:) retains the first equal maximum.
            let peak = first_maximum(&scores);
            let index = low + peak;
            let mut fraction = 0.0;
            if peak > 0 && peak + 1 < scores.len() {
                let left = scores[peak - 1];
                let center = scores[peak];
                let right = scores[peak + 1];
                let curvature = left - 2.0 * center + right;
                if curvature < -0.000_001 {
                    fraction = (0.5 * (left - right) / curvature).clamp(-0.5, 0.5);
                }
            }
            let split = query_bands.count() / 2;
            candidates.push(Candidate {
                offset_ms: self.start_ms + (index as f64 + fraction) * self.hop_ms - query_start_ms,
                score: scores[peak],
                first_half_score: self.score_part(&query_bands, index, 0, split),
                second_half_score: self.score_part(&query_bands, index, split, query_bands.count()),
                recent_score: self.score_part(
                    &query_bands,
                    index,
                    query_bands
                        .count()
                        .saturating_sub((1_000.0 / self.hop_ms) as usize),
                    query_bands.count(),
                ),
            });
        }

        // Stable sorting preserves the coarse-peak ordering on tied scores.
        candidates.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut independent: Vec<Candidate> = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if !independent
                .iter()
                .any(|other| (other.offset_ms - candidate.offset_ms).abs() < 750.0)
            {
                independent.push(candidate);
            }
        }
        MatchResult {
            candidates: independent,
        }
    }

    fn score_part(&self, query: &Bands, reference_start: usize, low: usize, high: usize) -> f64 {
        let mut score = 0.0;
        let floor = (high - low) as f64 * Self::VARIANCE_FLOOR;
        for band in 0..query.values.len() {
            let dot = dot_product(
                &self.fine.values[band][reference_start + low..reference_start + high],
                &query.values[band][low..high],
            );
            let reference_energy = self.fine.energy[band][reference_start + high]
                - self.fine.energy[band][reference_start + low];
            let query_energy = query.energy[band][high] - query.energy[band][low];
            score +=
                f64::from(dot) / (floor.max(reference_energy) * floor.max(query_energy)).sqrt();
        }
        score / query.values.len() as f64
    }
}

fn correlations(reference: &Bands, query: &Bands, low: usize, high: usize) -> Vec<f64> {
    let n = query.count();
    let mut scores = vec![0.0; high - low + 1];
    let floor = n as f64 * SpectralMatcher::VARIANCE_FLOOR;
    for band in 0..query.values.len() {
        let reference_values = &reference.values[band];
        let query_values = &query.values[band];
        let query_energy = floor.max(query.energy[band][n]);
        for (i, score) in scores.iter_mut().enumerate() {
            let start = low + i;
            let dot = dot_product(&reference_values[start..start + n], query_values);
            let reference_energy =
                floor.max(reference.energy[band][start + n] - reference.energy[band][start]);
            *score += f64::from(dot) / (query_energy * reference_energy).sqrt();
        }
    }
    let scale = 1.0 / query.values.len() as f64;
    for score in &mut scores {
        *score *= scale;
    }
    scores
}

/// Independent f32 accumulators allow portable LLVM vectorization without
/// requiring fast-math or a platform-specific DSP library. Accelerate may use a
/// different reduction order; fixture parity therefore compares with tolerance.
#[inline]
fn dot_product(left: &[f32], right: &[f32]) -> f32 {
    debug_assert_eq!(left.len(), right.len());
    let mut sums = [0.0_f32; 8];
    let (left_chunks, left_remainder) = left.as_chunks::<8>();
    let (right_chunks, right_remainder) = right.as_chunks::<8>();
    for (left, right) in left_chunks.iter().zip(right_chunks) {
        for lane in 0..8 {
            sums[lane] += left[lane] * right[lane];
        }
    }
    let mut sum = 0.0_f32;
    for value in sums {
        sum += value;
    }
    for (&left, &right) in left_remainder.iter().zip(right_remainder) {
        sum += left * right;
    }
    sum
}

fn first_maximum(scores: &[f64]) -> usize {
    let mut best = 0;
    for i in 1..scores.len() {
        if scores[i] > scores[best] {
            best = i;
        }
    }
    best
}

fn peaks(scores: &[f64], separation: usize, limit: usize) -> Vec<usize> {
    let mut result: Vec<usize> = Vec::with_capacity(limit);
    for _ in 0..limit {
        let mut best = None;
        for i in 0..scores.len() {
            if result
                .iter()
                .any(|&previous| previous.abs_diff(i) < separation)
            {
                continue;
            }
            if best.is_none_or(|previous| scores[i] > scores[previous]) {
                best = Some(i);
            }
        }
        let Some(best) = best else { break };
        result.push(best);
    }
    result
}

fn is_regular(frames: &[FeatureFrame], hop_ms: f64) -> bool {
    if !hop_ms.is_finite()
        || hop_ms <= 0.0
        || frames.len() <= 1
        || !frames[0].recorded_time_ms.is_finite()
    {
        return false;
    }
    for i in 1..frames.len() {
        let delta = frames[i].recorded_time_ms - frames[i - 1].recorded_time_ms;
        let expected = frames[0].recorded_time_ms + i as f64 * hop_ms;
        if !delta.is_finite()
            || (delta - hop_ms).abs() >= hop_ms * 0.05
            || (frames[i].recorded_time_ms - expected).abs() >= hop_ms * 0.05
        {
            return false;
        }
    }
    true
}

fn centered(frames: &[FeatureFrame], band_count: usize, radius: usize) -> Option<Vec<Vec<f32>>> {
    if !frames.iter().all(|frame| {
        frame.pcen_mel.len() == band_count
            && frame
                .pcen_mel
                .iter()
                .all(|&value| value.is_finite() && value >= 0.0)
    }) {
        return None;
    }
    let count = frames.len();
    let mut output = Vec::with_capacity(band_count);
    for band in 0..band_count {
        let values: Vec<f32> = frames
            .iter()
            .map(|frame| frame.pcen_mel[band].ln_1p())
            .collect();
        let mut sums = Vec::with_capacity(count + 1);
        sums.push(0.0);
        for &value in &values {
            sums.push(sums.last().unwrap() + f64::from(value));
        }
        let mut centered = Vec::with_capacity(count);
        for i in 0..count {
            let low = i.saturating_sub(radius);
            let high = count.min(i + radius + 1);
            let before = radius.saturating_sub(i);
            let after = (i + radius + 1).saturating_sub(count);
            let total = sums[high] - sums[low]
                + before as f64 * f64::from(values[0])
                + after as f64 * f64::from(values[count - 1]);
            centered.push(values[i] - (total / (2 * radius + 1) as f64) as f32);
        }
        output.push(centered);
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(count: usize) -> Vec<FeatureFrame> {
        let mut seed = 42_u32;
        (0..count)
            .map(|i| FeatureFrame {
                recorded_time_ms: i as f64 * 10.0,
                energy_dbfs: -20.0,
                pcen_mel: (0..8)
                    .map(|_| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        0.1 + (seed >> 8) as f32 / (1_u32 << 24) as f32 * 3.0
                    })
                    .collect(),
            })
            .collect()
    }

    fn window(mut frames: Vec<FeatureFrame>, start_ms: f64) -> FeatureWindow {
        for (i, frame) in frames.iter_mut().enumerate() {
            frame.recorded_time_ms = start_ms + i as f64 * 10.0;
        }
        FeatureWindow {
            start_recorded_time_ms: frames.first().unwrap().recorded_time_ms,
            endpoint_recorded_time_ms: frames.last().unwrap().recorded_time_ms,
            frames,
        }
    }

    #[test]
    fn finds_excerpt_with_capture_clock_offset() {
        let reference = frames(1_200);
        let matcher = SpectralMatcher::new(&reference, 10.0).unwrap();
        let query = window(reference[350..800].to_vec(), 1_234.0);
        let result = matcher.match_query(&query, None);
        let best = result.best().unwrap();
        let expected_offset = 3_500.0 - 1_234.0;
        assert!((best.offset_ms - expected_offset).abs() < 1.0, "{best:?}");
        assert!((best.score - 1.0).abs() < 0.000_001, "{best:?}");
        assert!(best.first_half_score > 0.999_99);
        assert!(best.second_half_score > 0.999_99);
        assert!(best.recent_score > 0.999_99);
        assert!(result.margin() > 0.5);

        let tracked = matcher.match_query(
            &query,
            Some((expected_offset - 100.0, expected_offset + 100.0)),
        );
        assert!((tracked.best().unwrap().offset_ms - expected_offset).abs() < 1.0);
        assert!(matcher
            .match_query(&query, Some((-10_000.0, -9_000.0)))
            .best()
            .is_none());
    }

    #[test]
    fn preserves_equal_repeated_passages_as_competing_candidates() {
        let mut reference = frames(1_400);
        let phrase = reference[..500].to_vec();
        for (i, frame) in phrase.iter().enumerate() {
            reference[800 + i].pcen_mel.clone_from(&frame.pcen_mel);
        }
        let matcher = SpectralMatcher::new(&reference, 10.0).unwrap();
        let query = window(phrase[..450].to_vec(), 0.0);
        let result = matcher.match_query(&query, None);
        assert!(result.candidates.len() >= 2);
        assert!((result.candidates[0].score - 1.0).abs() < 0.000_001);
        assert!((result.candidates[1].score - 1.0).abs() < 0.000_001);
        assert!(result.margin().abs() < 0.000_001);
        assert!((result.candidates[0].offset_ms - result.candidates[1].offset_ms).abs() > 7_900.0);
    }

    #[test]
    fn rejects_drifting_timestamps_and_invalid_feature_values() {
        let reference = frames(1_000);
        let matcher = SpectralMatcher::new(&reference, 10.0).unwrap();
        let mut query = window(reference[..450].to_vec(), 0.0);
        for (i, frame) in query.frames.iter_mut().enumerate() {
            frame.recorded_time_ms += i as f64 * 0.01;
        }
        assert!(!matcher.accepts_timeline(&query));
        assert!(matcher.match_query(&query, None).best().is_none());

        let mut invalid = reference.clone();
        invalid[100].pcen_mel[2] = f32::NAN;
        assert!(SpectralMatcher::new(&invalid, 10.0).is_none());
        let query = window(invalid[..450].to_vec(), 0.0);
        assert!(matcher.match_query(&query, None).best().is_none());
    }
}
