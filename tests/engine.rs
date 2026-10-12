use sonalign::{Configuration, Engine, FeatureFrame, FeatureWindow, State, WithholdReason};

// These independent spectral envelopes and cases are shared with the Swift
// engine regression suite; they do not depend on either feature extractor.
fn frames(count: usize, seed: usize) -> Vec<FeatureFrame> {
    (0..count)
        .map(|i| {
            let values = (0..24)
                .map(|band| {
                    let a = ((band + 3) * (seed + 7)) as f64;
                    let t = i as f64;
                    (0.5 * (t * (0.061 + a * 0.000_37) + a).sin()
                        + 0.5 * (t * (0.113 + a * 0.000_19) + t * t * 0.000_13).sin())
                    .exp() as f32
                })
                .collect();
            frame(i as f64 * 20.0, values)
        })
        .collect()
}

fn frame(recorded_time_ms: f64, pcen_mel: Vec<f32>) -> FeatureFrame {
    FeatureFrame {
        recorded_time_ms,
        energy_dbfs: -20.0,
        pcen_mel,
    }
}

fn capture(
    reference: &[FeatureFrame],
    offset: usize,
    count: usize,
    noisy: bool,
) -> Vec<FeatureFrame> {
    (0..count)
        .map(|i| {
            let values = reference[i + offset]
                .pcen_mel
                .iter()
                .enumerate()
                .map(|(band, value)| {
                    if noisy {
                        ((band % 4) as f32 * 0.3
                            + value * (0.5 + (band % 5) as f64 * 0.2) as f32
                            + (0.15 * ((i * (band + 17)) as f64 * 0.123).sin()) as f32)
                            .max(0.0)
                    } else {
                        *value
                    }
                })
                .collect();
            frame(i as f64 * 20.0, values)
        })
        .collect()
}

fn window(frames: &[FeatureFrame], end: usize) -> FeatureWindow {
    FeatureWindow::new(frames[end.saturating_sub(250)..=end].to_vec()).unwrap()
}

fn engine(reference: &[FeatureFrame]) -> Engine {
    Engine::new(reference, 20.0, Configuration::default())
}

#[test]
fn noisy_spectral_changes_align_without_landmarks() {
    let reference = frames(2_000, 1);
    let query = capture(&reference, 200, 500, true);
    let mut engine = engine(&reference);
    let mut last = None;
    for end in (125..=450).step_by(25) {
        last = Some(engine.process(&window(&query, end), end as f64 * 20.0));
    }
    let result = last.unwrap();
    assert_eq!(result.state, State::Locked);
    assert!((result.estimate.unwrap().offset_ms - 4_000.0).abs() < 8.0);
}

#[test]
fn stationary_and_unrelated_audio_cannot_lock() {
    let reference = frames(1_500, 1);
    let flat = (0..500)
        .map(|i| frame(i as f64 * 20.0, vec![2.0; 24]))
        .collect::<Vec<_>>();
    let wrong = capture(&frames(1_500, 97), 200, 500, false);
    for query in [flat, wrong] {
        let mut engine = engine(&reference);
        for end in (125..=490).step_by(25) {
            let result = engine.process(&window(&query, end), end as f64 * 20.0);
            assert!(!matches!(result.state, State::Locked | State::Confirmed));
        }
    }
}

#[test]
fn repeated_passage_remains_ambiguous() {
    let passage = frames(500, 1);
    let mut reference = passage.clone();
    reference.extend(
        passage
            .iter()
            .map(|f| frame(f.recorded_time_ms + 10_000.0, f.pcen_mel.clone())),
    );
    let query = capture(&reference, 100, 350, false);
    let mut engine = engine(&reference);
    for end in (125..=340).step_by(25) {
        let result = engine.process(&window(&query, end), end as f64 * 20.0);
        assert_ne!(result.state, State::Locked);
        assert!(result.estimate.is_none());
    }
}

#[test]
fn repeated_window_does_not_manufacture_fresh_evidence() {
    let reference = frames(1_500, 1);
    let query = capture(&reference, 100, 400, false);
    let mut engine = engine(&reference);
    let same = window(&query, 150);
    for elapsed in (3_000..=10_000).step_by(100) {
        let result = engine.process(&same, elapsed as f64);
        assert!(!matches!(result.state, State::Confirmed | State::Locked));
        assert_eq!(result.diagnostics.spectral.fresh_evidence_ms, 0.0);
    }
}

#[test]
fn callback_cadence_does_not_change_evidence_requirements() {
    let reference = frames(1_500, 1);
    let query = capture(&reference, 100, 500, false);
    let mut confirmed_times = Vec::new();
    for step in [5, 50] {
        let mut engine = engine(&reference);
        for end in (50..=450).step_by(step) {
            let result = engine.process(&window(&query, end), end as f64 * 20.0);
            if let Some(confirmed) = result.confirmed_lock_elapsed_ms {
                confirmed_times.push(confirmed);
                break;
            }
        }
    }
    assert_eq!(confirmed_times.len(), 2);
    assert!(confirmed_times.iter().all(|time| *time >= 3_500.0));
    assert!((confirmed_times[0] - confirmed_times[1]).abs() <= 1_000.0);
}

#[test]
fn confirmation_is_final_without_waiting_for_a_longer_window() {
    let reference = frames(2_000, 1);
    let query = capture(&reference, 200, 500, false);
    let mut engine = engine(&reference);
    let longest_window_ms = window(&query, 400).duration_ms();
    for end in (0..=400).step_by(5) {
        let query_window = window(&query, end);
        let result = engine.process(&query_window, end as f64 * 20.0);
        if result.final_lock_elapsed_ms.is_some() {
            // Final comes with the confirming evidence, while the window still grows.
            assert_eq!(
                result.confirmed_lock_elapsed_ms,
                result.final_lock_elapsed_ms
            );
            assert!(query_window.duration_ms() < longest_window_ms);
            return;
        }
    }
    panic!("no final lock");
}

#[test]
fn weak_search_at_the_pending_position_keeps_the_estimate() {
    let reference = frames(2_000, 1);
    let matching = capture(&reference, 200, 500, false);
    let unrelated = capture(&frames(2_000, 97), 200, 500, false);
    // Faint matching audio for 2.3 s, then the same audio fainter still. Later
    // searches stay at its position but are too weak to confirm it, and soon too
    // weak to pass the provisional gate.
    let query = matching
        .iter()
        .zip(&unrelated)
        .enumerate()
        .map(|(i, (m, u))| {
            let weight: f32 = if i < 115 { 0.45 } else { 0.1 };
            let values = m.pcen_mel.iter().zip(&u.pcen_mel);
            frame(
                m.recorded_time_ms,
                values
                    .map(|(a, b)| weight * a + (1.0 - weight) * b)
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    let mut engine = engine(&reference);
    let (mut held, mut released) = (0, false);
    for end in (0..=495).step_by(5) {
        let result = engine.process(&window(&query, end), end as f64 * 20.0);
        assert!(!matches!(result.state, State::Confirmed | State::Locked));
        let spectral = &result.diagnostics.spectral;
        if spectral.global_search && spectral.correlation < 0.25 {
            // A weak search still at the pending position keeps the estimate,
            // until an ambiguous search or staleness releases it.
            match result.estimate {
                Some(estimate) => {
                    assert!((estimate.offset_ms - 4_000.0).abs() < 8.0);
                    held += 1;
                }
                None => released = true,
            }
        }
    }
    assert!(held > 0 && released);
}

#[test]
fn seek_relocks_and_does_not_keep_old_audio_alive() {
    let reference = frames(2_500, 1);
    let query = (0..900)
        .map(|i| {
            frame(
                i as f64 * 20.0,
                reference[i + if i < 400 { 100 } else { 700 }]
                    .pcen_mel
                    .clone(),
            )
        })
        .collect::<Vec<_>>();
    let mut engine = engine(&reference);
    let mut saw_loss = false;
    let mut latest = None;
    for end in (125..=850).step_by(25) {
        let result = engine.process(&window(&query, end), end as f64 * 20.0);
        if end >= 525 && result.state != State::Locked {
            saw_loss = true;
        }
        if end >= 550 && result.state == State::Locked {
            assert!((result.estimate.as_ref().unwrap().offset_ms - 14_000.0).abs() < 8.0);
        }
        latest = Some(result);
    }
    assert!(saw_loss);
    let latest = latest.unwrap();
    assert_eq!(latest.state, State::Locked);
    assert!((latest.estimate.unwrap().offset_ms - 14_000.0).abs() < 8.0);
}

#[test]
fn discontinuity_and_silence_release_accepted_offset() {
    let reference = frames(1_500, 1);
    let query = capture(&reference, 100, 500, false);
    let mut engine = engine(&reference);
    for end in (125..=400).step_by(25) {
        engine.process(&window(&query, end), end as f64 * 20.0);
    }
    let silence = FeatureWindow::new(
        (0..250)
            .map(|i| FeatureFrame {
                recorded_time_ms: 10_000.0 + i as f64 * 20.0,
                energy_dbfs: -120.0,
                pcen_mel: vec![0.0; 24],
            })
            .collect(),
    )
    .unwrap();
    let result = engine.process(&silence, 15_000.0);
    assert_eq!(result.state, State::Lost);
    assert_eq!(
        result.withhold_reason,
        Some(WithholdReason::InsufficientEnergy)
    );
    assert!(result.estimate.is_none());
}

#[test]
fn short_tracking_window_retains_lock_after_acquisition() {
    let reference = frames(1_500, 1);
    let query = capture(&reference, 100, 500, false);
    let mut engine = engine(&reference);
    for end in (125..=350).step_by(25) {
        engine.process(&window(&query, end), end as f64 * 20.0);
    }
    let short = FeatureWindow::new(query[300..=400].to_vec()).unwrap();
    let result = engine.process(&short, 8_000.0);
    assert_eq!(result.state, State::Locked);
    assert!((result.estimate.unwrap().offset_ms - 2_000.0).abs() < 8.0);
}

#[test]
fn timestamp_skew_invalidates_lock_including_fast_reuse_path() {
    let reference = frames(1_500, 1);
    let clean = capture(&reference, 100, 500, false);
    let mut engine = engine(&reference);
    for end in (125..=400).step_by(25) {
        engine.process(&window(&clean, end), end as f64 * 20.0);
    }
    for shift in [0.0, -1_150.0] {
        let mut engine = engine.clone();
        let skew = FeatureWindow::new(
            (0..250)
                .map(|i| {
                    frame(
                        4_000.0 + i as f64 * 20.0 * 1.04 + shift,
                        reference[i + 300].pcen_mel.clone(),
                    )
                })
                .collect(),
        )
        .unwrap();
        let result = engine.process(&skew, 9_200.0 + shift);
        assert_eq!(result.state, State::Lost);
        assert_eq!(result.withhold_reason, Some(WithholdReason::LostSignal));
        assert!(result.estimate.is_none());
    }
}

#[test]
fn small_playback_clock_drift_is_tracked_in_audio_content() {
    let reference = frames(2_000, 1);
    let query = (0..1_000)
        .map(|i| {
            let position = 100.0 + i as f64 * 1.001;
            let lower = position as usize;
            let fraction = (position - lower as f64) as f32;
            let values = reference[lower]
                .pcen_mel
                .iter()
                .zip(&reference[lower + 1].pcen_mel)
                .map(|(a, b)| a + fraction * (b - a))
                .collect();
            frame(i as f64 * 20.0, values)
        })
        .collect::<Vec<_>>();
    let mut engine = engine(&reference);
    for end in (125..=950).step_by(25) {
        let result = engine.process(&window(&query, end), end as f64 * 20.0);
        if end > 350 {
            assert_eq!(result.state, State::Locked);
            assert!(
                (result.estimate.unwrap().offset_ms - (2_000.0 + end as f64 * 0.02)).abs() < 8.0
            );
        }
    }
}

#[test]
fn lost_track_requires_global_disambiguation_before_recovery() {
    let original = frames(2_500, 1);
    let reference = original
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let values = if (1_500..2_000).contains(&i) {
                &original[i - 1_000].pcen_mel
            } else {
                &f.pcen_mel
            };
            frame(f.recorded_time_ms, values.clone())
        })
        .collect::<Vec<_>>();
    let unrelated = frames(1_000, 83);
    let query = (0..900)
        .map(|i| {
            let values = if (400..550).contains(&i) {
                &unrelated[i].pcen_mel
            } else {
                &reference[i + 100].pcen_mel
            };
            frame(i as f64 * 20.0, values.clone())
        })
        .collect::<Vec<_>>();
    let mut engine = engine(&reference);
    let mut lost = false;
    for end in (125..=875).step_by(5) {
        let result = engine.process(&window(&query, end), end as f64 * 20.0);
        if (300..400).contains(&end) {
            assert_eq!(result.state, State::Locked);
        }
        if end >= 500 && result.state != State::Locked {
            lost = true;
        }
        if end >= 550 && lost {
            assert_ne!(result.state, State::Locked);
        }
    }
    assert!(lost);
}

#[test]
fn unavailable_reference_is_reported_without_an_estimate() {
    let mut engine = engine(&[]);
    let query = FeatureWindow::new(frames(251, 1)).unwrap();
    let result = engine.process(&query, 5_000.0);
    assert_eq!(result.state, State::Failed);
    assert_eq!(
        result.withhold_reason,
        Some(WithholdReason::IndexUnavailable)
    );
    assert!(result.estimate.is_none());
}

#[test]
fn reference_band_mismatch_invalidates_the_fast_reuse_path() {
    let reference = frames(1500, 1);
    let query = (0..600)
        .map(|i| frame(i as f64 * 20.0, reference[i + 100].pcen_mel.clone()))
        .collect::<Vec<_>>();
    let mut engine = engine(&reference);
    for end in (125..=450).step_by(25) {
        engine.process(&window(&query, end), end as f64 * 20.0);
    }
    let locked = engine.process(&window(&query, 450), 9000.0);
    assert_eq!(locked.state, State::Locked);
    let mut incompatible = window(&query, 451).frames().to_vec();
    for frame in &mut incompatible {
        frame.pcen_mel.pop();
    }
    let result = engine.process(&FeatureWindow::new(incompatible).unwrap(), 9020.0);
    assert_eq!(result.state, State::Lost);
    assert_eq!(result.withhold_reason, Some(WithholdReason::LostSignal));
    assert!(result.estimate.is_none());
}
