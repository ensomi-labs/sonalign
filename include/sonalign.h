#ifndef SONALIGN_H
#define SONALIGN_H
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

/* ABI 1. Engine calls allocate/search: use a worker, never an audio callback.
 * A handle is owned by one caller; serialize all access including destruction.
 * All buffers are borrowed for the call only and copied. PCEN must be generated
 * with identical reference/query settings. Timestamps are analysis-window ends
 * in recorded audio time (milliseconds), not callback-arrival time.
 * Changing the size/layout of these structs requires a new ABI version. */
typedef struct SonalignHandle SonalignHandle;
typedef struct {
    const double *recorded_times_ms;
    const double *energies_dbfs; /* optional; NULL supplies -20 dBFS */
    const float *pcen;          /* row-major frame_count * band_count */
    size_t frame_count;
    size_t band_count;
} SonalignFeatureBatch;

enum { SONALIGN_OK = 0, SONALIGN_INVALID_ARGUMENT = 1, SONALIGN_PANIC = 2, SONALIGN_POISONED = 3 };
enum { SONALIGN_READY = 0, SONALIGN_LISTENING = 1, SONALIGN_LOCKING = 2,
    SONALIGN_CONFIRMED = 3, SONALIGN_LOCKED = 4, SONALIGN_DRIFTING = 5,
    SONALIGN_RELOCKING = 6, SONALIGN_LOST = 7, SONALIGN_FAILED = 8 };
enum { SONALIGN_PHASE_NONE = 0, SONALIGN_PHASE_PROVISIONAL = 1, SONALIGN_PHASE_CONFIRMED = 2, SONALIGN_PHASE_FINAL = 3 };
enum { SONALIGN_STAGE_READINESS = 0, SONALIGN_STAGE_LANDMARK_COARSE = 1,
    SONALIGN_STAGE_FAST_TIMING_VERIFY = 2, SONALIGN_STAGE_ROBUST_VERIFY = 3,
    SONALIGN_STAGE_TRACKING = 4, SONALIGN_STAGE_RELOCK = 5 };
enum { SONALIGN_REASON_NONE = 0, SONALIGN_INSUFFICIENT_DURATION = 1,
    SONALIGN_INSUFFICIENT_ENERGY = 2, SONALIGN_INSUFFICIENT_ACTIVE_FRAMES = 3,
    SONALIGN_INSUFFICIENT_LANDMARK_EVIDENCE = 4, SONALIGN_WEAK_ALIGNMENT_PEAK = 5,
    SONALIGN_AMBIGUOUS_OFFSET = 6, SONALIGN_UNSTABLE_TRACKING_RESIDUAL = 7,
    SONALIGN_LOST_SIGNAL = 8, SONALIGN_DECODE_FAILED = 9, SONALIGN_INDEX_UNAVAILABLE = 10 };
typedef struct {
    uint32_t state, phase, stage, withhold_reason, has_estimate, global_search;
    double query_endpoint_recorded_time_ms;
    double reference_time_ms, offset_ms; /* NaN if has_estimate == 0 */
    double confidence;                 /* score, not calibrated probability */
    double first_provisional_lock_elapsed_ms, confirmed_lock_elapsed_ms, final_lock_elapsed_ms;
    double correlation, competing_peak_margin, fresh_evidence_ms, coast_ms;
    /* Unset milestones and non-global competing_peak_margin are NaN. */
} SonalignResultV1;

uint32_t sonalign_abi_version(void);
/* Output pointers must be aligned, writable and not alias any input buffers.
 * Errors leave outputs untouched. Nonzero status must be checked before use.
 * PANIC poisons an existing handle; destroy it before creating a replacement.
 * hop_ms must match the features' grid (default extraction: 512/48000*1000).
 * Irregular reference timing yields FAILED / INDEX_UNAVAILABLE on process. */
int32_t sonalign_create(SonalignFeatureBatch reference, double hop_ms, SonalignHandle **out_handle);
int32_t sonalign_process(SonalignHandle *handle, SonalignFeatureBatch query, double elapsed_ms, SonalignResultV1 *out_result);
void sonalign_destroy(SonalignHandle *handle); /* NULL accepted; otherwise exactly once */
#ifdef __cplusplus
}
#endif
#endif
