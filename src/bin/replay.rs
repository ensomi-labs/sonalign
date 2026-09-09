use sonalign::{Configuration, Engine, FeatureFrame, FeatureWindow};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    error::Error,
    fs,
    io::{BufRead, BufReader, BufWriter, Write},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

// Stage file output beside its destination. A malformed case cannot truncate
// the previous result, nor can a hard-link alias overwrite the input inode.
struct Output {
    writer: Box<dyn Write>,
    paths: Option<(std::path::PathBuf, std::path::PathBuf)>,
}
impl Output {
    fn new(path: Option<String>) -> Result<Self, Box<dyn Error>> {
        if let Some(path) = path {
            let destination = std::path::PathBuf::from(path);
            let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let temporary = destination.with_file_name(format!(
                ".sonalign-{}-{nonce}.tmp",
                std::process::id()
            ));
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            Ok(Self {
                writer: Box::new(BufWriter::new(file)),
                paths: Some((temporary, destination)),
            })
        } else {
            Ok(Self {
                writer: Box::new(BufWriter::new(std::io::stdout())),
                paths: None,
            })
        }
    }
    fn finish(mut self) -> Result<(), Box<dyn Error>> {
        self.writer.flush()?;
        drop(std::mem::replace(
            &mut self.writer,
            Box::new(std::io::sink()),
        ));
        if let Some((temporary, destination)) = &self.paths {
            fs::rename(temporary, destination)?;
        }
        self.paths = None;
        Ok(())
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        // Close the file before unlinking it, including on platforms which do
        // not permit removing an open file.
        drop(std::mem::replace(
            &mut self.writer,
            Box::new(std::io::sink()),
        ));
        if let Some((temporary, _)) = &self.paths {
            let _ = fs::remove_file(temporary);
        }
    }
}

#[derive(Deserialize)]
struct Source {
    #[serde(rename = "timesPath")]
    times_path: String,
    #[serde(rename = "pcenPath")]
    pcen_path: String,
    #[serde(rename = "energyPath")]
    energy_path: Option<String>,
    #[serde(rename = "hopMS")]
    hop_ms: Option<f64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Call {
    start_index: usize,
    end_index: usize,
    #[serde(rename = "elapsedMS")]
    elapsed_ms: f64,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    reference: Source,
    query: Source,
    calls: Vec<Call>,
    bands: Option<usize>,
    #[serde(rename = "energyDBFS")]
    energy_dbfs: Option<f64>,
}

fn doubles(path: &str) -> Result<Vec<f64>, Box<dyn Error>> {
    let bytes = fs::read(path)?;
    if bytes.len() % 8 != 0 {
        return Err(format!("invalid f64LE file: {path}").into());
    }
    Ok(bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| f64::from_le_bytes(*b))
        .collect())
}
fn frames(source: &Source, bands: usize, energy: f64) -> Result<Vec<FeatureFrame>, Box<dyn Error>> {
    let times = doubles(&source.times_path)?;
    let bytes = fs::read(&source.pcen_path)?;
    let expected = times
        .len()
        .checked_mul(bands)
        .and_then(|n| n.checked_mul(4))
        .ok_or("feature size overflow")?;
    if bands == 0 || bytes.len() != expected {
        return Err("PCEN dimensions do not match times/bands".into());
    }
    let energies = source.energy_path.as_deref().map(doubles).transpose()?;
    if energies.as_ref().is_some_and(|e| e.len() != times.len()) {
        return Err("energy dimensions mismatch".into());
    }
    let mut output = Vec::with_capacity(times.len());
    for (i, recorded_time_ms) in times.into_iter().enumerate() {
        let start = i * bands * 4;
        let pcen_mel = bytes[start..start + bands * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        output.push(FeatureFrame {
            recorded_time_ms,
            energy_dbfs: energies.as_ref().map_or(energy, |e| e[i]),
            pcen_mel,
        });
    }
    Ok(output)
}
fn replay(case: Case) -> Result<Value, Box<dyn Error>> {
    let bands = case.bands.unwrap_or(24);
    let energy = case.energy_dbfs.unwrap_or(-20.0);
    let reference = frames(&case.reference, bands, energy)?;
    let query = frames(&case.query, bands, energy)?;
    let hop = case.reference.hop_ms.unwrap_or(512.0 / 48_000.0 * 1000.0);
    if !hop.is_finite() || hop <= 0.0 {
        return Err("invalid hopMS".into());
    }
    let started = Instant::now();
    let mut engine = Engine::new(&reference, hop, Configuration::default());
    let index_ns = started.elapsed().as_nanos() as u64;
    let mut events = Vec::with_capacity(case.calls.len());
    let mut process_ns = 0u64;
    for call in case.calls {
        if call.start_index > call.end_index
            || call.end_index >= query.len()
            || !call.elapsed_ms.is_finite()
        {
            return Err(format!("invalid call in {}", case.id).into());
        }
        let window = FeatureWindow::new(query[call.start_index..=call.end_index].to_vec())?;
        let started = Instant::now();
        let snapshot = engine.process(&window, call.elapsed_ms);
        let ns = started.elapsed().as_nanos() as u64;
        process_ns += ns;
        let mut event = serde_json::to_value(snapshot)?;
        let object = event.as_object_mut().unwrap();
        let diagnostics = object.remove("diagnostics").unwrap();
        object.extend(diagnostics.as_object().unwrap().clone());
        let candidates = object["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| json!({"offsetMS": c["offsetMS"], "score": c["combinedDenseScore"]}))
            .collect();
        object.insert("candidates".into(), Value::Array(candidates));
        object.insert(
            "endpointMS".into(),
            json!(window.endpoint_recorded_time_ms()),
        );
        object.insert("elapsedMS".into(), json!(call.elapsed_ms));
        object.insert("processNS".into(), json!(ns));
        events.push(event);
    }
    Ok(json!({"id": case.id, "events": events, "processNS": process_ns, "indexNS": index_ns}))
}
fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mut input = None;
    let mut output = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--cases" => input = Some(args.next().ok_or("--cases requires a path")?),
            "--output" => output = Some(args.next().ok_or("--output requires a path")?),
            "--help" | "-h" => {
                println!("sonalign-replay --cases cases.jsonl --output results.jsonl\nOmit either path to use stdin/stdout. Inputs are PCEN features, not PCM audio.");
                return Ok(());
            }
            _ => return Err(format!("unknown argument {arg}").into()),
        }
    }
    if input.is_some() && input == output {
        return Err("input and output must differ".into());
    }
    if let (Some(input), Some(output)) = (&input, &output) {
        if fs::canonicalize(output).ok().as_ref() == Some(&fs::canonicalize(input)?) {
            return Err("input and output refer to the same file".into());
        }
    }
    let reader: Box<dyn BufRead> = match input {
        Some(path) => Box::new(BufReader::new(fs::File::open(path)?)),
        None => Box::new(BufReader::new(std::io::stdin())),
    };
    let mut output = Output::new(output)?;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let result = replay(serde_json::from_str(&line)?)?;
        serde_json::to_writer(&mut output.writer, &result)?;
        output.writer.write_all(b"\n")?;
        output.writer.flush()?;
    }
    output.finish()
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
