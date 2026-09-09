use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn replay_never_truncates_input_aliases_or_previous_results_on_error() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "sonalign-cli-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let input = directory.join("cases.jsonl");
    fs::write(&input, "malformed case\n").unwrap();
    let alias = directory.join(".").join("cases.jsonl");
    let executable = env!("CARGO_BIN_EXE_sonalign-replay");
    let result = Command::new(executable)
        .arg("--cases")
        .arg(&input)
        .arg("--output")
        .arg(alias)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read_to_string(&input).unwrap(), "malformed case\n");
    let output = directory.join("results.jsonl");
    fs::write(&output, "previous valid results\n").unwrap();
    let result = Command::new(executable)
        .arg("--cases")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(
        fs::read_to_string(&output).unwrap(),
        "previous valid results\n"
    );
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
    fs::remove_dir_all(directory).unwrap();
}
