//! Versioned C ABI for a single worker-owned engine. See include/sonalign.h.
//! Buffers are borrowed only for the duration of a call and copied into Rust.
//! Callers must provide valid, aligned pointers of the declared lengths. Calls
//! on one handle must be serialized, including destruction. Never call on an
//! audio callback thread. C ABI errors leave output arguments untouched.

use crate::{Configuration, Engine, FeatureFrame, FeatureWindow, Snapshot};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub const ABI_VERSION: u32 = 1;
pub const OK: i32 = 0;
pub const INVALID_ARGUMENT: i32 = 1;
pub const PANIC: i32 = 2;
pub const POISONED: i32 = 3;

pub struct Handle {
    engine: Engine,
    bands: usize,
    poisoned: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FeatureBatch {
    pub recorded_times_ms: *const f64,
    /// Null means -20 dBFS for every frame; provide actual energy for readiness.
    pub energies_dbfs: *const f64,
    /// Row-major frame_count × band_count nonnegative finite PCEN values.
    pub pcen: *const f32,
    pub frame_count: usize,
    pub band_count: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ResultV1 {
    pub state: u32,
    pub phase: u32,
    pub stage: u32,
    pub withhold_reason: u32,
    pub has_estimate: u32,
    pub global_search: u32,
    pub query_endpoint_recorded_time_ms: f64,
    /// NaN when has_estimate == 0.
    pub reference_time_ms: f64,
    pub offset_ms: f64,
    pub confidence: f64,
    /// Unset milestones and competing margin are NaN.
    pub first_provisional_lock_elapsed_ms: f64,
    pub confirmed_lock_elapsed_ms: f64,
    pub final_lock_elapsed_ms: f64,
    pub correlation: f64,
    pub competing_peak_margin: f64,
    pub fresh_evidence_ms: f64,
    pub coast_ms: f64,
}

unsafe fn read_batch(batch: FeatureBatch) -> Result<Vec<FeatureFrame>, i32> {
    let count = batch.frame_count;
    let bands = batch.band_count;
    let elements = count.checked_mul(bands).ok_or(INVALID_ARGUMENT)?;
    if count == 0
        || bands == 0
        || count > isize::MAX as usize / 8
        || elements > isize::MAX as usize / 4
        || batch.recorded_times_ms.is_null()
        || batch.pcen.is_null()
        || !(batch.recorded_times_ms as usize).is_multiple_of(std::mem::align_of::<f64>())
        || !(batch.pcen as usize).is_multiple_of(std::mem::align_of::<f32>())
        || (!batch.energies_dbfs.is_null()
            && !(batch.energies_dbfs as usize).is_multiple_of(std::mem::align_of::<f64>()))
    {
        return Err(INVALID_ARGUMENT);
    }
    let times = unsafe { std::slice::from_raw_parts(batch.recorded_times_ms, count) };
    let pcen = unsafe { std::slice::from_raw_parts(batch.pcen, elements) };
    let energies = if batch.energies_dbfs.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(batch.energies_dbfs, count) })
    };
    if times.iter().any(|v| !v.is_finite())
        || pcen.iter().any(|v| !v.is_finite() || *v < 0.0)
        || energies.is_some_and(|e| e.iter().any(|v| v.is_nan() || *v == f64::INFINITY))
    {
        return Err(INVALID_ARGUMENT);
    }
    Ok(times
        .iter()
        .enumerate()
        .map(|(i, &recorded_time_ms)| FeatureFrame {
            recorded_time_ms,
            energy_dbfs: energies.map_or(-20.0, |e| e[i]),
            pcen_mel: pcen[i * bands..(i + 1) * bands].to_vec(),
        })
        .collect())
}

#[no_mangle]
pub extern "C" fn sonalign_abi_version() -> u32 {
    ABI_VERSION
}

/// Create an engine with Swift V2 defaults. Reference features are copied.
/// # Safety
/// Batch pointers must be valid for their declared lengths; out_handle must be
/// writable and aligned. The returned handle must be freed exactly once.
#[no_mangle]
pub unsafe extern "C" fn sonalign_create(
    reference: FeatureBatch,
    hop_ms: f64,
    out_handle: *mut *mut Handle,
) -> i32 {
    if out_handle.is_null() || !hop_ms.is_finite() || hop_ms <= 0.0 {
        return INVALID_ARGUMENT;
    }
    match catch_unwind(AssertUnwindSafe(|| {
        let frames = unsafe { read_batch(reference) }?;
        let handle = Box::new(Handle {
            engine: Engine::new(&frames, hop_ms, Configuration::default()),
            bands: reference.band_count,
            poisoned: false,
        });
        unsafe {
            out_handle.write(Box::into_raw(handle));
        }
        Ok::<_, i32>(())
    })) {
        Ok(Ok(())) => OK,
        Ok(Err(code)) => code,
        Err(_) => PANIC,
    }
}

/// Process a rolling query window. An internal panic poisons the handle.
/// # Safety
/// Handle must be live and exclusively accessed; batch pointers must be valid;
/// out_result must point to writable, aligned ResultV1 storage without aliasing
/// input buffers or the handle.
#[no_mangle]
pub unsafe extern "C" fn sonalign_process(
    handle: *mut Handle,
    query: FeatureBatch,
    elapsed_ms: f64,
    out_result: *mut ResultV1,
) -> i32 {
    if handle.is_null() || out_result.is_null() || !elapsed_ms.is_finite() {
        return INVALID_ARGUMENT;
    }
    let handle = unsafe { &mut *handle };
    if handle.poisoned {
        return POISONED;
    }
    if query.band_count != handle.bands {
        return INVALID_ARGUMENT;
    }
    match catch_unwind(AssertUnwindSafe(|| {
        let window =
            FeatureWindow::new(unsafe { read_batch(query) }?).map_err(|_| INVALID_ARGUMENT)?;
        let snapshot = handle.engine.process(&window, elapsed_ms);
        let result = result_v1(&snapshot, window.endpoint_recorded_time_ms());
        unsafe {
            out_result.write(result);
        }
        Ok::<_, i32>(())
    })) {
        Ok(Ok(())) => OK,
        Ok(Err(code)) => code,
        Err(_) => {
            handle.poisoned = true;
            PANIC
        }
    }
}

/// Free a handle; null is accepted.
/// # Safety
/// A nonnull pointer must have been returned by sonalign_create and must not
/// have been freed before. No concurrent calls may use it.
#[no_mangle]
pub unsafe extern "C" fn sonalign_destroy(handle: *mut Handle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

fn result_v1(snapshot: &Snapshot, endpoint: f64) -> ResultV1 {
    use crate::{LockPhase as P, Stage as G, State as S, WithholdReason as W};
    let spectral = &snapshot.diagnostics.spectral;
    ResultV1 {
        state: match snapshot.state {
            S::Ready => 0,
            S::Listening => 1,
            S::Locking => 2,
            S::Confirmed => 3,
            S::Locked => 4,
            S::Drifting => 5,
            S::Relocking => 6,
            S::Lost => 7,
            S::Failed => 8,
        },
        phase: match snapshot.phase {
            P::None => 0,
            P::Provisional => 1,
            P::Confirmed => 2,
            P::Final => 3,
        },
        stage: match snapshot.stage {
            G::Readiness => 0,
            G::LandmarkCoarse => 1,
            G::FastTimingVerify => 2,
            G::RobustVerify => 3,
            G::Tracking => 4,
            G::Relock => 5,
        },
        withhold_reason: match snapshot.withhold_reason {
            None => 0,
            Some(W::InsufficientDuration) => 1,
            Some(W::InsufficientEnergy) => 2,
            Some(W::InsufficientActiveFrames) => 3,
            Some(W::InsufficientLandmarkEvidence) => 4,
            Some(W::WeakAlignmentPeak) => 5,
            Some(W::AmbiguousOffset) => 6,
            Some(W::UnstableTrackingResidual) => 7,
            Some(W::LostSignal) => 8,
            Some(W::DecodeFailed) => 9,
            Some(W::IndexUnavailable) => 10,
        },
        has_estimate: u32::from(snapshot.estimate.is_some()),
        global_search: u32::from(spectral.global_search),
        query_endpoint_recorded_time_ms: endpoint,
        reference_time_ms: snapshot
            .estimate
            .as_ref()
            .map_or(f64::NAN, |e| e.reference_time_ms),
        offset_ms: snapshot.estimate.as_ref().map_or(f64::NAN, |e| e.offset_ms),
        confidence: snapshot.confidence,
        first_provisional_lock_elapsed_ms: snapshot
            .first_provisional_lock_elapsed_ms
            .unwrap_or(f64::NAN),
        confirmed_lock_elapsed_ms: snapshot.confirmed_lock_elapsed_ms.unwrap_or(f64::NAN),
        final_lock_elapsed_ms: snapshot.final_lock_elapsed_ms.unwrap_or(f64::NAN),
        correlation: spectral.correlation,
        competing_peak_margin: spectral.competing_peak_margin.unwrap_or(f64::NAN),
        fresh_evidence_ms: spectral.fresh_evidence_ms,
        coast_ms: spectral.coast_ms,
    }
}
