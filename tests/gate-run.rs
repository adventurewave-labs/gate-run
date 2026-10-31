use std::fs;
use std::process::Command;

fn tmpdir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("gate-run-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn write_gate(dir: &std::path::Path, name: &str, code: i32) -> String {
    let p = dir.join(name);
    fs::write(&p, format!("#!/bin/sh\necho \"gate stdout (verdict noise)\"\nexit {code}\n")).unwrap();
    p.to_str().unwrap().to_string()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_gate-run"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn on_ok_runs_only_on_approval() {
    let d = tmpdir("approve");
    let gate = write_gate(&d, "gate-ok.sh", 0);
    let marker = d.join("pushed");

    let (code, _, stderr) = run(&[
        "--on-ok",
        &format!("touch {}", marker.display()),
        "--",
        "sh",
        &gate,
    ]);
    assert_eq!(code, 0);
    assert!(marker.exists(), "on-ok must run when the gate approves");
    assert!(stderr.contains("APPROVED"));
}

#[test]
fn on_ok_skipped_on_revise() {
    let d = tmpdir("revise");
    let gate = write_gate(&d, "gate-revise.sh", 1);
    let marker = d.join("pushed");

    let (code, _, stderr) = run(&[
        "--on-ok",
        &format!("touch {}", marker.display()),
        "--",
        "sh",
        &gate,
    ]);
    assert_eq!(code, 1, "gate-run must propagate the gate's REVISE exit code");
    assert!(!marker.exists(), "on-ok must NOT run on REVISE — this is the 3x incident");
    assert!(stderr.contains("SKIPPED"), "skip must be loud");
}

#[test]
fn on_ok_skipped_on_ownership_refusal() {
    let d = tmpdir("ownership");
    let gate = write_gate(&d, "gate-owned.sh", 4);
    let marker = d.join("pushed");

    let (code, _, _) = run(&[
        "--on-ok",
        &format!("touch {}", marker.display()),
        "--",
        "sh",
        &gate,
    ]);
    assert_eq!(code, 4);
    assert!(!marker.exists(), "exit 4 (ownership refusal) must also block on-ok");
}

#[test]
fn on_fail_hook_runs_on_revise() {
    let d = tmpdir("onfail");
    let gate = write_gate(&d, "gate-fail.sh", 1);
    let alert = d.join("alerted");

    let (code, _, _) = run(&[
        "--on-ok",
        "true",
        "--on-fail",
        &format!("touch {}", alert.display()),
        "--",
        "sh",
        &gate,
    ]);
    assert_eq!(code, 1);
    assert!(alert.exists(), "on-fail hook must run on nonzero");
}

#[test]
fn unusable_gate_command_is_127() {
    let (code, _, stderr) = run(&["--", "definitely-not-a-real-binary-xyz", "--base", "main"]);
    assert_eq!(code, 127);
    assert!(stderr.contains("cannot run gate"));
}

#[test]
fn missing_gate_command_is_usage_error() {
    let (code, _, _) = run(&["--on-ok", "true"]);
    assert_eq!(code, 64);
}
