use sonalign::{FeatureFrame, FeatureWindow};

fn frame(values: Vec<f32>) -> FeatureFrame {
    FeatureFrame {
        recorded_time_ms: 10.0,
        energy_dbfs: -20.0,
        pcen_mel: values,
    }
}

#[test]
fn invalid_features_cannot_enter_the_reused_snapshot_path() {
    for values in [vec![], vec![f32::NAN], vec![f32::INFINITY], vec![-1.0]] {
        assert!(FeatureWindow::new(vec![frame(values)]).is_err());
    }
    assert!(FeatureWindow::new(vec![frame(vec![1.0]), frame(vec![1.0, 2.0])]).is_err());
    let mut silent = frame(vec![0.0]);
    silent.energy_dbfs = f64::NEG_INFINITY;
    assert!(FeatureWindow::new(vec![silent]).is_ok());
}
