use crate::matcher::{MatchResult, SpectralMatcher};
use crate::types::{
    CandidateDiagnostics, Configuration, Diagnostics, Estimate, FeatureFrame, FeatureWindow,
    LockPhase, Snapshot, SpectralDiagnostics, Stage, State, WithholdReason,
};

#[derive(Clone, Debug)]
struct Hypothesis {
    offset_ms: f64,
    first_evidence_ms: f64,
    last_evidence_ms: f64,
    correlation: f64,
}

/// Aligns successive feature windows to one known reference.
///
/// Decisions use recorded audio time. Re-reading an overlapping window cannot
/// create fresh evidence, extend a coast, or accelerate confidence decay.
/// `elapsed_ms` records milestones only; it does not affect alignment decisions.
#[derive(Clone, Debug)]
pub struct Engine {
    matcher: Option<SpectralMatcher>,
    configuration: Configuration,
    hop_ms: f64,
    pending: Option<Hypothesis>,
    accepted: Option<Hypothesis>,
    last_endpoint_ms: Option<f64>,
    last_global_search_ms: f64,
    last_snapshot: Option<Snapshot>,
    first_provisional_ms: Option<f64>,
    confirmed_ms: Option<f64>,
    final_ms: Option<f64>,
}

impl Engine {
    /// Builds the reusable reference index.
    ///
    /// # Panics
    /// Panics if `configuration` fails [`Configuration::validate`]. An invalid
    /// reference or hop is reported as `IndexUnavailable` by [`Self::process`].
    pub fn new(
        reference_frames: &[FeatureFrame],
        hop_ms: f64,
        configuration: Configuration,
    ) -> Self {
        configuration
            .validate()
            .expect("invalid engine configuration");
        Self {
            matcher: SpectralMatcher::new(reference_frames, hop_ms),
            configuration,
            hop_ms,
            pending: None,
            accepted: None,
            last_endpoint_ms: None,
            last_global_search_ms: f64::NEG_INFINITY,
            last_snapshot: None,
            first_provisional_ms: None,
            confirmed_ms: None,
            final_ms: None,
        }
    }

    /// Processes one window and projects any estimate to its recorded endpoint.
    ///
    /// # Panics
    /// Panics if `elapsed_ms` is not finite.
    pub fn process(&mut self, query: &FeatureWindow, elapsed_ms: f64) -> Snapshot {
        assert!(elapsed_ms.is_finite(), "elapsed_ms must be finite");
        let endpoint = query.endpoint_recorded_time_ms;
        let hop = self.hop_ms;
        if let Some(previous) = self.last_endpoint_ms {
            if endpoint < previous || query.start_recorded_time_ms > previous + 2.0 * hop {
                self.pending = None;
                self.accepted = None;
                self.last_global_search_ms = f64::NEG_INFINITY;
                self.last_snapshot = None;
            } else if endpoint - previous < 90.0
                && self
                    .matcher
                    .as_ref()
                    .is_none_or(|matcher| matcher.accepts_timeline(query))
            {
                if let Some(previous_snapshot) = &self.last_snapshot {
                    let mut snapshot = previous_snapshot.clone();
                    snapshot.estimate = snapshot
                        .estimate
                        .map(|estimate| Self::estimate(query, estimate.offset_ms));
                    snapshot.first_provisional_lock_elapsed_ms = self.first_provisional_ms;
                    snapshot.confirmed_lock_elapsed_ms = self.confirmed_ms;
                    snapshot.final_lock_elapsed_ms = self.final_ms;
                    return snapshot;
                }
            }
        }
        self.last_endpoint_ms = Some(endpoint);
        let active = query
            .frames
            .iter()
            .filter(|frame| {
                frame.energy_dbfs >= self.configuration.minimum_active_frame_energy_dbfs
            })
            .count() as f64
            / query.frames.len() as f64;
        let energy = average_energy_dbfs(&query.frames);
        let minimum_duration = if self.confirmed_ms.is_none() {
            self.configuration.minimum_readiness_duration_ms
        } else {
            self.configuration
                .minimum_readiness_duration_ms
                .min(self.configuration.tracking_query_duration_ms - hop)
        };
        let readiness = match &self.matcher {
            None => Some(WithholdReason::IndexUnavailable),
            Some(matcher) => {
                if query.duration_ms() < minimum_duration {
                    Some(WithholdReason::InsufficientDuration)
                } else if !matcher.accepts_timeline(query) {
                    Some(WithholdReason::LostSignal)
                } else if energy < self.configuration.minimum_average_energy_dbfs {
                    Some(WithholdReason::InsufficientEnergy)
                } else if active < self.configuration.minimum_active_frame_fraction {
                    Some(WithholdReason::InsufficientActiveFrames)
                } else {
                    None
                }
            }
        };

        if let Some(reason) = readiness {
            self.pending = None;
            // Silence is an observed loss of audio, not support for the old position.
            self.accepted = None;
            let state = if reason == WithholdReason::IndexUnavailable {
                State::Failed
            } else if self.confirmed_ms.is_none() {
                State::Listening
            } else {
                State::Lost
            };
            return self.publish(
                query,
                active,
                &MatchResult { candidates: vec![] },
                false,
                Some(reason),
                state,
                None,
                None,
                0.0,
            );
        }
        let matcher = self
            .matcher
            .as_ref()
            .expect("Readiness checked the reference index");

        // Centering trims 250 ms at either end. Evidence ends at the actual
        // supported endpoint, rather than the newest uncentered feature frame.
        let evidence_ms = endpoint - (250.0 / hop).round() * hop;
        let mut result = MatchResult { candidates: vec![] };
        let mut locally_supported = false;
        let mut innovation = None;
        if let Some(mut track) = self
            .accepted
            .clone()
            .filter(|track| evidence_ms - track.last_evidence_ms <= 1_500.0)
        {
            result = matcher.match_query(
                query,
                Some((track.offset_ms - 100.0, track.offset_ms + 100.0)),
            );
            if let Some(best) = result.best() {
                if best.score >= 0.15
                    && best.recent_score >= 0.12
                    && (best.offset_ms - track.offset_ms).abs() <= 80.0
                {
                    innovation = Some(best.offset_ms - track.offset_ms);
                    let dt = (evidence_ms - track.last_evidence_ms).max(0.0);
                    let gain = 1.0 - (-dt / 300.0).exp();
                    track.offset_ms += gain * (best.offset_ms - track.offset_ms);
                    track.last_evidence_ms = evidence_ms;
                    track.correlation = best.score;
                    self.accepted = Some(track);
                    locally_supported = true;
                }
            }
        } else {
            // A dormant position is not independent evidence. After expiration,
            // only global verification may establish another lock.
            self.accepted = None;
        }

        let global_interval = if locally_supported { 2_000.0 } else { 500.0 };
        let global = endpoint - self.last_global_search_ms >= global_interval;
        if global {
            result = matcher.match_query(query, None);
            self.last_global_search_ms = endpoint;
            if let Some(best) = result.best().filter(|best| {
                best.score >= 0.25
                    && result.margin() >= 0.06
                    && best.first_half_score.min(best.second_half_score) >= 0.10
            }) {
                if self.first_provisional_ms.is_none() {
                    self.first_provisional_ms = Some(elapsed_ms);
                }
                let first_evidence_ms = self
                    .pending
                    .as_ref()
                    .filter(|old| {
                        (old.offset_ms - best.offset_ms).abs() <= 80.0
                            && evidence_ms - old.last_evidence_ms <= 2_500.0
                    })
                    .map_or(evidence_ms, |old| old.first_evidence_ms);
                self.pending = Some(Hypothesis {
                    offset_ms: best.offset_ms,
                    first_evidence_ms,
                    last_evidence_ms: evidence_ms,
                    correlation: best.score,
                });
                let fresh = evidence_ms - first_evidence_ms;
                let strong_early = best.score >= 0.45
                    && result.margin() >= 0.12
                    && best.first_half_score.min(best.second_half_score) >= 0.20;
                let long_enough =
                    query.duration_ms() >= self.configuration.minimum_final_query_duration_ms;
                if fresh >= 1_000.0 - hop
                    && best.recent_score >= 0.15
                    && (long_enough || strong_early)
                {
                    if self.confirmed_ms.is_none() {
                        self.confirmed_ms = Some(elapsed_ms);
                    }
                    if long_enough && self.final_ms.is_none() {
                        self.final_ms = Some(elapsed_ms);
                    }
                    if self.accepted.as_ref().is_none_or(|track| {
                        (track.offset_ms - best.offset_ms).abs() > 80.0 || !locally_supported
                    }) {
                        self.accepted = Some(Hypothesis {
                            offset_ms: best.offset_ms,
                            first_evidence_ms: evidence_ms,
                            last_evidence_ms: evidence_ms,
                            correlation: best.score,
                        });
                    }
                    locally_supported = true;
                }
            } else {
                self.pending = None;
            }
        }

        if let Some(track) = &self.accepted {
            if evidence_ms - track.last_evidence_ms <= 1_500.0 {
                return self.publish(
                    query,
                    active,
                    &result,
                    global,
                    if locally_supported {
                        None
                    } else {
                        Some(WithholdReason::WeakAlignmentPeak)
                    },
                    if self.final_ms.is_none() {
                        State::Confirmed
                    } else {
                        State::Locked
                    },
                    Some(track.offset_ms),
                    innovation,
                    (evidence_ms - track.last_evidence_ms).max(0.0),
                );
            }
        }
        let ambiguous = result.best().is_some() && result.margin() < 0.06;
        self.publish(
            query,
            active,
            &result,
            global,
            Some(if ambiguous {
                WithholdReason::AmbiguousOffset
            } else {
                WithholdReason::WeakAlignmentPeak
            }),
            if self.confirmed_ms.is_none() {
                State::Locking
            } else {
                State::Relocking
            },
            if self.confirmed_ms.is_none() {
                self.pending.as_ref().map(|pending| pending.offset_ms)
            } else {
                None
            },
            None,
            0.0,
        )
    }

    fn estimate(query: &FeatureWindow, offset_ms: f64) -> Estimate {
        Estimate {
            query_endpoint_recorded_time_ms: query.endpoint_recorded_time_ms,
            reference_time_ms: query.endpoint_recorded_time_ms + offset_ms,
            offset_ms,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn publish(
        &mut self,
        query: &FeatureWindow,
        active: f64,
        result: &MatchResult,
        global: bool,
        reason: Option<WithholdReason>,
        state: State,
        offset_ms: Option<f64>,
        innovation: Option<f64>,
        coast_ms: f64,
    ) -> Snapshot {
        let publishing_track = state == State::Locked || state == State::Confirmed;
        let supported = publishing_track && coast_ms == 0.0;
        let spectral = SpectralDiagnostics {
            global_search: global,
            correlation: result.best().map_or(0.0, |best| best.score),
            competing_peak_margin: global.then(|| result.margin()),
            first_half_correlation: result.best().map_or(0.0, |best| best.first_half_score),
            second_half_correlation: result.best().map_or(0.0, |best| best.second_half_score),
            recent_correlation: result.best().map_or(0.0, |best| best.recent_score),
            fresh_evidence_ms: self.pending.as_ref().map_or(0.0, |pending| {
                (pending.last_evidence_ms - pending.first_evidence_ms).max(0.0)
            }),
            coast_ms,
        };
        let diagnostics = Diagnostics {
            query_duration_ms: query.duration_ms(),
            active_frame_fraction: active,
            coarse_ambiguous: global && result.best().is_some() && result.margin() < 0.06,
            dense_margin: if global { result.margin() } else { 0.0 },
            offset_stability_ms: innovation.map(f64::abs),
            track_innovation_ms: innovation,
            track_count: usize::from(self.accepted.is_some()),
            offset_tracker_confirmed: publishing_track,
            offset_tracker_stable: supported,
            candidates: result
                .candidates
                .iter()
                .map(|candidate| CandidateDiagnostics {
                    offset_ms: candidate.offset_ms,
                    coarse_offset_ms: candidate.offset_ms,
                    comparable_frame_count: query
                        .frames
                        .len()
                        .saturating_sub(((250.0 / self.hop_ms).round() as usize).saturating_mul(2)),
                    coverage_ratio: ((query.duration_ms() - 500.0) / query.duration_ms().max(1.0))
                        .max(0.0),
                    combined_dense_score: candidate.score,
                })
                .collect(),
            spectral,
        };
        let phase = if self.final_ms.is_some() {
            LockPhase::Final
        } else if self.confirmed_ms.is_some() {
            LockPhase::Confirmed
        } else if self.first_provisional_ms.is_some() {
            LockPhase::Provisional
        } else {
            LockPhase::None
        };
        let correlation = if publishing_track {
            self.accepted
                .as_ref()
                .map_or(0.0, |track| track.correlation)
        } else {
            result.best().map_or(0.0, |best| best.score)
        };
        let snapshot = Snapshot {
            state,
            phase,
            stage: if publishing_track {
                Stage::Tracking
            } else if global {
                Stage::RobustVerify
            } else {
                Stage::Readiness
            },
            estimate: offset_ms.map(|offset| Self::estimate(query, offset)),
            withhold_reason: reason,
            confidence: if offset_ms.is_none() {
                0.0
            } else {
                (correlation * (1.0 - coast_ms / 1_500.0)).clamp(0.0, 1.0)
            },
            diagnostics,
            first_provisional_lock_elapsed_ms: self.first_provisional_ms,
            confirmed_lock_elapsed_ms: self.confirmed_ms,
            final_lock_elapsed_ms: self.final_ms,
        };
        self.last_snapshot = Some(snapshot.clone());
        snapshot
    }
}

fn average_energy_dbfs(frames: &[FeatureFrame]) -> f64 {
    if frames.is_empty() {
        return f64::NEG_INFINITY;
    }
    let mean_power = frames
        .iter()
        .map(|frame| 10.0_f64.powf(frame.energy_dbfs / 10.0))
        .sum::<f64>()
        / frames.len() as f64;
    if mean_power > 0.0 {
        10.0 * mean_power.log10()
    } else {
        f64::NEG_INFINITY
    }
}
