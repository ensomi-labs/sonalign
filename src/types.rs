use serde::{Deserialize, Serialize};

/// The feature timestamp is the end of its analysis window, in recorded audio time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureFrame {
    pub recorded_time_ms: f64,
    pub energy_dbfs: f64,
    pub pcen_mel: Vec<f32>,
}

/// An owned, nonempty window. Endpoints are always derived from its frames.
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureWindow {
    pub(crate) frames: Vec<FeatureFrame>,
    pub(crate) start_recorded_time_ms: f64,
    pub(crate) endpoint_recorded_time_ms: f64,
}

impl FeatureWindow {
    pub fn new(frames: Vec<FeatureFrame>) -> Result<Self, InputError> {
        if frames.is_empty() {
            return Err(InputError("feature window is empty"));
        }
        let bands = frames[0].pcen_mel.len();
        if bands == 0
            || frames.iter().any(|frame| {
                frame.pcen_mel.len() != bands
                    || frame
                        .pcen_mel
                        .iter()
                        .any(|value| !value.is_finite() || *value < 0.0)
            })
        {
            return Err(InputError(
                "PCEN frames must have matching nonempty bands and finite nonnegative values",
            ));
        }
        if frames.iter().any(|f| {
            !f.recorded_time_ms.is_finite()
                || f.energy_dbfs.is_nan()
                || f.energy_dbfs == f64::INFINITY
        }) {
            return Err(InputError(
                "timestamps must be finite and energies cannot be NaN or +infinity",
            ));
        }
        Ok(Self {
            start_recorded_time_ms: frames[0].recorded_time_ms,
            endpoint_recorded_time_ms: frames[frames.len() - 1].recorded_time_ms,
            frames,
        })
    }
    pub fn frames(&self) -> &[FeatureFrame] {
        &self.frames
    }
    pub fn start_recorded_time_ms(&self) -> f64 {
        self.start_recorded_time_ms
    }
    pub fn endpoint_recorded_time_ms(&self) -> f64 {
        self.endpoint_recorded_time_ms
    }
    pub fn duration_ms(&self) -> f64 {
        self.endpoint_recorded_time_ms - self.start_recorded_time_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputError(pub &'static str);
impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InputError {}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Configuration {
    pub minimum_readiness_duration_ms: f64,
    pub tracking_query_duration_ms: f64,
    pub minimum_active_frame_energy_dbfs: f64,
    pub minimum_average_energy_dbfs: f64,
    pub minimum_active_frame_fraction: f64,
}
impl Default for Configuration {
    fn default() -> Self {
        Self {
            minimum_readiness_duration_ms: 2500.0,
            tracking_query_duration_ms: 2000.0,
            minimum_active_frame_energy_dbfs: -70.0,
            minimum_average_energy_dbfs: -62.0,
            minimum_active_frame_fraction: 0.35,
        }
    }
}
impl Configuration {
    pub fn validate(&self) -> Result<(), InputError> {
        let durations = [
            self.minimum_readiness_duration_ms,
            self.tracking_query_duration_ms,
        ];
        if durations.iter().any(|v| !v.is_finite() || *v <= 0.0)
            || !self.minimum_active_frame_energy_dbfs.is_finite()
            || !self.minimum_average_energy_dbfs.is_finite()
            || !self.minimum_active_frame_fraction.is_finite()
            || !(0.0..=1.0).contains(&self.minimum_active_frame_fraction)
        {
            return Err(InputError("invalid engine configuration"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Ready,
    Listening,
    Locking,
    Confirmed,
    Locked,
    Drifting,
    Relocking,
    Lost,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LockPhase {
    None,
    Provisional,
    Confirmed,
    Final,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Readiness,
    LandmarkCoarse,
    FastTimingVerify,
    RobustVerify,
    Tracking,
    Relock,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WithholdReason {
    InsufficientDuration,
    InsufficientEnergy,
    InsufficientActiveFrames,
    InsufficientLandmarkEvidence,
    WeakAlignmentPeak,
    AmbiguousOffset,
    UnstableTrackingResidual,
    LostSignal,
    DecodeFailed,
    IndexUnavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    #[serde(rename = "queryEndpointRecordedTimeMS")]
    pub query_endpoint_recorded_time_ms: f64,
    #[serde(rename = "referenceTimeMS")]
    pub reference_time_ms: f64,
    #[serde(rename = "offsetMS")]
    pub offset_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpectralDiagnostics {
    pub global_search: bool,
    pub correlation: f64,
    pub competing_peak_margin: Option<f64>,
    pub first_half_correlation: f64,
    pub second_half_correlation: f64,
    pub recent_correlation: f64,
    #[serde(rename = "freshEvidenceMS")]
    pub fresh_evidence_ms: f64,
    #[serde(rename = "coastMS")]
    pub coast_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDiagnostics {
    #[serde(rename = "offsetMS")]
    pub offset_ms: f64,
    #[serde(rename = "coarseOffsetMS")]
    pub coarse_offset_ms: f64,
    pub comparable_frame_count: usize,
    pub coverage_ratio: f64,
    pub combined_dense_score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    #[serde(rename = "queryDurationMS")]
    pub query_duration_ms: f64,
    pub active_frame_fraction: f64,
    pub coarse_ambiguous: bool,
    pub dense_margin: f64,
    #[serde(rename = "offsetStabilityMS")]
    pub offset_stability_ms: Option<f64>,
    #[serde(rename = "trackInnovationMS")]
    pub track_innovation_ms: Option<f64>,
    pub track_count: usize,
    pub offset_tracker_confirmed: bool,
    pub offset_tracker_stable: bool,
    pub candidates: Vec<CandidateDiagnostics>,
    pub spectral: SpectralDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub state: State,
    pub phase: LockPhase,
    pub stage: Stage,
    pub estimate: Option<Estimate>,
    pub withhold_reason: Option<WithholdReason>,
    pub confidence: f64,
    pub diagnostics: Diagnostics,
    #[serde(rename = "firstProvisionalLockElapsedMS")]
    pub first_provisional_lock_elapsed_ms: Option<f64>,
    #[serde(rename = "confirmedLockElapsedMS")]
    pub confirmed_lock_elapsed_ms: Option<f64>,
    #[serde(rename = "finalLockElapsedMS")]
    pub final_lock_elapsed_ms: Option<f64>,
}
