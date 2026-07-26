//! 集成测试：加载管线 TOML → 执行 builtin 节点 → 验证输出
//!
//! Wave 4 / Agent F

use std::fs;
use std::path::{Path, PathBuf};

use ep_core::pipeline::dag::{Pipeline, ValidationError};
use ep_core::pipeline::runner::PipelineRunnerImpl;
use ep_core::types::{PipelineRunner, TaskStatus};

// ─── Helpers ────────────────────────────────────────────────────────────────

fn unique_temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ep_integ_pipe_{}_{}_{}",
        label,
        std::process::id(),
        uuid_suffix(),
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn uuid_suffix() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut h);
    format!("{:x}", h.finish() & 0xFFFF_FFFF)
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

/// Check if ffmpeg is available on PATH
fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok()
}

// ─── Test 1: Load pipeline from TOML ────────────────────────────────────────

#[test]
fn test_load_pipeline_from_toml() {
    let dir = unique_temp_dir("load_toml");

    let toml_content = r#"
[pipeline]
id = "integ-test-pipeline"
name = "Integration Test Pipeline"
description = "Testing TOML loading"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
label = "Input"
params = { accept = "audio" }

[[nodes]]
id = "process"
kind = "builtin"
builtin = "ffmpeg"
label = "Process"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
label = "Output"

[[edges]]
from = ["input", "output"]
to = ["process", "input"]

[[edges]]
from = ["process", "output"]
to = ["output", "input"]
"#;

    // Write to file and load
    let pipeline_file = dir.join("test_pipeline.toml");
    fs::write(&pipeline_file, toml_content).unwrap();

    let pipeline = Pipeline::from_toml(&pipeline_file).expect("should parse pipeline TOML");

    assert_eq!(pipeline.id, "integ-test-pipeline");
    assert_eq!(pipeline.name, "Integration Test Pipeline");
    assert_eq!(pipeline.nodes.len(), 3);
    assert_eq!(pipeline.edges.len(), 2);

    // Validate should pass
    assert!(pipeline.validate().is_ok(), "pipeline should be valid");

    // Topological sort should produce 3 layers
    let layers = pipeline.topological_layers().unwrap();
    assert_eq!(layers.len(), 3);
    assert_eq!(layers[0], vec!["input"]);
    assert_eq!(layers[1], vec!["process"]);
    assert_eq!(layers[2], vec!["output"]);

    cleanup(&dir);
}

// ─── Test 2: Execute FileInput → FileOutput pipeline ────────────────────────

#[test]
fn test_execute_file_pipeline() {
    let work_dir = unique_temp_dir("file_exec");

    // Create input file
    let input_file = work_dir.join("source_data.txt");
    fs::write(&input_file, "integration test data for file pipeline").unwrap();

    let output_file = work_dir.join("output_data.txt");

    let toml_str = format!(
        r#"
[pipeline]
id = "file-pipe"
name = "File Pipeline"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["output", "input"]
"#,
        input_file.to_string_lossy().replace('\\', "/"),
        output_file.to_string_lossy().replace('\\', "/"),
    );

    let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
    let mut runner = PipelineRunnerImpl::new(work_dir.clone());

    let result = runner.execute(&pipeline, &work_dir);
    assert!(result.is_ok(), "file pipeline should succeed: {:?}", result);

    // Verify output file exists and has correct content
    assert!(output_file.exists(), "output file should exist");
    let content = fs::read_to_string(&output_file).unwrap();
    assert_eq!(content, "integration test data for file pipeline");

    // Verify task status
    assert_eq!(*runner.task_status(), TaskStatus::Completed);

    // Verify node statuses
    assert!(matches!(
        runner.node_status("input"),
        Some(ep_core::pipeline::NodeState::Completed { .. })
    ));
    assert!(matches!(
        runner.node_status("output"),
        Some(ep_core::pipeline::NodeState::Completed { .. })
    ));

    cleanup(&work_dir);
}

// ─── Test 3: Execute FFmpeg pipeline (skip if ffmpeg unavailable) ───────────

#[test]
fn test_execute_ffmpeg_pipeline() {
    if !ffmpeg_available() {
        eprintln!("SKIP: ffmpeg not available on PATH");
        return;
    }

    let work_dir = unique_temp_dir("ffmpeg_exec");

    // Create a dummy input file
    let input_file = work_dir.join("dummy_input.txt");
    fs::write(&input_file, "dummy").unwrap();

    let output_file = work_dir.join("ffmpeg_output.raw");

    let toml_str = format!(
        r#"
[pipeline]
id = "ffmpeg-pipe"
name = "FFmpeg Pipeline"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "encode"
kind = "builtin"
builtin = "ffmpeg"
params = {{ args = ["-f", "lavfi", "-i", "testsrc=duration=1:size=32x32:rate=1", "-frames:v", "1", "-f", "rawvideo", "-y"], output = "{}" }}

[[nodes]]
id = "save"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["encode", "input"]

[[edges]]
from = ["encode", "output"]
to = ["save", "input"]
"#,
        input_file.to_string_lossy().replace('\\', "/"),
        output_file.to_string_lossy().replace('\\', "/"),
        work_dir.join("final_output.raw").to_string_lossy().replace('\\', "/"),
    );

    let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
    let mut runner = PipelineRunnerImpl::new(work_dir.clone());

    let result = runner.execute(&pipeline, &work_dir);
    assert!(result.is_ok(), "ffmpeg pipeline should succeed: {:?}", result);

    // Verify the ffmpeg output file was created
    assert!(output_file.exists(), "ffmpeg output should exist");

    assert_eq!(*runner.task_status(), TaskStatus::Completed);

    cleanup(&work_dir);
}

// ─── Test 4: Invalid TOML should return error ───────────────────────────────

#[test]
fn test_pipeline_with_invalid_toml() {
    // Completely invalid TOML
    let result = Pipeline::from_toml_str("this is not valid TOML {{{}}}");
    assert!(result.is_err(), "invalid TOML should fail to parse");

    // Valid TOML but invalid pipeline (cycle)
    let cyclic_toml = r#"
[pipeline]
id = "cyclic"
name = "Cyclic"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "a"
kind = "builtin"
builtin = "ffmpeg"

[[nodes]]
id = "b"
kind = "builtin"
builtin = "ffmpeg"

[[edges]]
from = ["input", "output"]
to = ["a", "input"]

[[edges]]
from = ["a", "output"]
to = ["b", "input"]

[[edges]]
from = ["b", "output"]
to = ["a", "input"]
"#;

    let pipeline = Pipeline::from_toml_str(cyclic_toml).unwrap();
    let errors = pipeline.validate().unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(e, ValidationError::CycleDetected)),
        "should detect cycle"
    );

    // Valid TOML but missing file_input
    let no_input_toml = r#"
[pipeline]
id = "no-input"
name = "No Input"

[[nodes]]
id = "process"
kind = "builtin"
builtin = "ffmpeg"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["process", "output"]
to = ["output", "input"]
"#;

    let pipeline = Pipeline::from_toml_str(no_input_toml).unwrap();
    let errors = pipeline.validate().unwrap_err();
    assert!(
        errors.iter().any(|e| matches!(e, ValidationError::NoFileInput)),
        "should detect missing file_input"
    );
}

// ─── Test 5: Multi-step linear pipeline (3 nodes) ───────────────────────────

#[test]
fn test_linear_three_node_pipeline() {
    let work_dir = unique_temp_dir("linear3");

    let input_file = work_dir.join("input.txt");
    let mid_output = work_dir.join("mid.txt");
    let final_output = work_dir.join("final.txt");

    fs::write(&input_file, "three-node linear test").unwrap();

    let toml_str = format!(
        r#"
[pipeline]
id = "linear3"
name = "Three Node Linear"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"
params = {{ path = "{}" }}

[[nodes]]
id = "mid"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[nodes]]
id = "final"
kind = "builtin"
builtin = "file_output"
params = {{ path = "{}" }}

[[edges]]
from = ["input", "output"]
to = ["mid", "input"]

[[edges]]
from = ["mid", "output"]
to = ["final", "input"]
"#,
        input_file.to_string_lossy().replace('\\', "/"),
        mid_output.to_string_lossy().replace('\\', "/"),
        final_output.to_string_lossy().replace('\\', "/"),
    );

    let pipeline = Pipeline::from_toml_str(&toml_str).unwrap();
    let mut runner = PipelineRunnerImpl::new(work_dir.clone());

    let result = runner.execute(&pipeline, &work_dir);
    assert!(result.is_ok(), "linear 3-node pipeline should succeed: {:?}", result);

    assert_eq!(*runner.task_status(), TaskStatus::Completed);

    // Both intermediate and final outputs should exist
    assert!(mid_output.exists(), "mid output should exist");
    assert!(final_output.exists(), "final output should exist");

    let content = fs::read_to_string(&final_output).unwrap();
    assert_eq!(content, "three-node linear test");

    cleanup(&work_dir);
}
