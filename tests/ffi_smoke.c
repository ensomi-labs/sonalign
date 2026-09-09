#include "sonalign.h"
#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>

/* A real C consumer exercises ABI layout, ownership, estimates and errors. */
int main(void) {
    enum { REFERENCE_FRAMES = 2000, QUERY_FRAMES = 1300, BANDS = 24, SHIFT = 350 };
    const double hop = 512.0 / 48000.0 * 1000.0;
    double *rt = malloc(REFERENCE_FRAMES * sizeof(double));
    double *qt = malloc(QUERY_FRAMES * sizeof(double));
    double *energy = malloc(QUERY_FRAMES * sizeof(double));
    float *rp = malloc(REFERENCE_FRAMES * BANDS * sizeof(float));
    float *qp = malloc(QUERY_FRAMES * BANDS * sizeof(float));
    assert(rt && qt && energy && rp && qp);
    uint32_t random = 42;
    for (size_t i = 0; i < REFERENCE_FRAMES; ++i) {
        rt[i] = (i + 2) * hop;
        for (size_t b = 0; b < BANDS; ++b) {
            random = random * 1664525u + 1013904223u;
            rp[i * BANDS + b] = (float)(random >> 8) / 16777216.0f;
        }
    }
    for (size_t i = 0; i < QUERY_FRAMES; ++i) {
        qt[i] = (i + 2) * hop;
        energy[i] = -20;
        for (size_t b = 0; b < BANDS; ++b) qp[i * BANDS + b] = rp[(i + SHIFT) * BANDS + b];
    }
    assert(sonalign_abi_version() == 1);
    SonalignHandle *handle = NULL;
    SonalignFeatureBatch reference = {rt, NULL, rp, REFERENCE_FRAMES, BANDS};
    assert(sonalign_create(reference, hop, &handle) == SONALIGN_OK && handle);
    SonalignResultV1 result = {0};
    for (size_t end = 150; end <= 1100; end += 50) {
        size_t start = end > 468 ? end - 468 : 0;
        SonalignFeatureBatch query = {qt + start, energy + start, qp + start * BANDS, end - start + 1, BANDS};
        assert(sonalign_process(handle, query, qt[end], &result) == SONALIGN_OK);
    }
    assert(result.state == SONALIGN_LOCKED && result.phase == SONALIGN_PHASE_FINAL);
    assert(result.has_estimate && fabs(result.offset_ms - SHIFT * hop) < 1.0);
    assert(fabs(result.reference_time_ms - result.query_endpoint_recorded_time_ms - result.offset_ms) < 1e-8);
    assert(isfinite(result.confirmed_lock_elapsed_ms));
    SonalignFeatureBatch invalid = {qt, NULL, qp, QUERY_FRAMES, BANDS - 1};
    result.state = 999;
    assert(sonalign_process(handle, invalid, 15000, &result) == SONALIGN_INVALID_ARGUMENT);
    assert(result.state == 999); /* Error leaves output untouched. */
    for (size_t i = 0; i < QUERY_FRAMES; ++i) energy[i] = -100;
    SonalignFeatureBatch quiet = {qt + 800, energy + 800, qp + 800 * BANDS, 469, BANDS};
    assert(sonalign_process(handle, quiet, qt[1268], &result) == SONALIGN_OK);
    assert(result.state == SONALIGN_LOST && result.withhold_reason == SONALIGN_INSUFFICIENT_ENERGY);
    assert(!result.has_estimate && isnan(result.offset_ms));
    sonalign_destroy(handle);
    sonalign_destroy(NULL);
    free(rt); free(qt); free(energy); free(rp); free(qp);
    puts("C ABI: acquisition, timestamped offset, silence, errors and ownership passed");
    return 0;
}
