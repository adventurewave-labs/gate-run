//! gate-run — run a gate, then make the verdict impossible to pipe away.
//!
//! A review gate that exits 0 only when shipping is allowed (turbo-flow's
//! rig-lite gate.sh, your own script) has one job: its exit code IS the
//! decision. But a shell pipe eats that code (`gate | tail && push` ships a
//! rejected diff). gate-run moves the follow-up action inside the process
//! that holds the verdict, so even piping gate-run's own output cannot
//! trigger the push on a rejection.

use std::process::{Command, ExitCode, Stdio};

const USAGE: &str = "\
gate-run — run a gate and keep its verdict un-maskable.

gate-run assumes exactly one thing about your gate: exit 0 means allowed to
ship. (turbo-flow's gate.sh contract for reference: 0 APPROVED · 1 REVISE ·
2 error · 4 round cap.)

USAGE:
  gate-run [--on-ok CMD] [--on-fail CMD] -- <gate command...>

  --on-ok CMD    run CMD (via sh -c) only if the gate exits 0; exit code becomes CMD's
  --on-fail CMD  run CMD (via sh -c) if the gate exits nonzero, before the skip notice
  --             the gate command and its arguments (required)

Example:
  gate-run --on-ok 'git push' -- ./gate.sh --base main

Exit: the gate's exit code (or the --on-ok command's) · 64 = bad usage · 127 = gate command unusable
";

fn verdict_name(code: u8) -> &'static str {
    match code {
        0 => "APPROVED",
        1 => "REVISE (or unparseable verdict — fail closed)",
        2 => "error",
        3 => "selftest failure",
        4 => "ownership refusal",
        _ => "nonzero — treat as REVISE-or-worse, fail closed",
    }
}

fn run_hook(cmd: &str, label: &str) -> Option<u8> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    match status {
        Ok(s) => Some(s.code().unwrap_or(125) as u8),
        Err(e) => {
            eprintln!("gate-run: --{label} hook failed to spawn: {e}");
            Some(126)
        }
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut on_ok: Option<String> = None;
    let mut on_fail: Option<String> = None;
    let mut gate: Vec<String> = Vec::new();

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--on-ok" => {
                i += 1;
                match argv.get(i) {
                    Some(v) if !v.is_empty() => on_ok = Some(v.clone()),
                    _ => {
                        eprint!("gate-run: --on-ok needs a command\n\n{USAGE}");
                        return ExitCode::from(64);
                    }
                }
            }
            "--on-fail" => {
                i += 1;
                match argv.get(i) {
                    Some(v) if !v.is_empty() => on_fail = Some(v.clone()),
                    _ => {
                        eprint!("gate-run: --on-fail needs a command\n\n{USAGE}");
                        return ExitCode::from(64);
                    }
                }
            }
            "--" => {
                gate.extend(argv[i + 1..].iter().cloned());
                break;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::from(0);
            }
            other => {
                eprint!("gate-run: unknown argument '{other}'\n\n{USAGE}");
                return ExitCode::from(64);
            }
        }
        i += 1;
    }

    if gate.is_empty() {
        eprint!("gate-run: no gate command given after '--'\n\n{USAGE}");
        return ExitCode::from(64);
    }

    let program = &gate[0];
    let status = Command::new(program)
        .args(&gate[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    let code = match status {
        Ok(s) => match s.code() {
            Some(c) => c as u8,
            None => {
                eprintln!("gate-run: gate killed by signal — fail closed");
                125
            }
        },
        Err(e) => {
            eprintln!("gate-run: cannot run gate '{program}': {e}");
            return ExitCode::from(127);
        }
    };

    eprintln!("gate-run: gate exited {code} — {}", verdict_name(code));

    if code == 0 {
        if let Some(cmd) = &on_ok {
            eprintln!("gate-run: APPROVED — running --on-ok");
            if let Some(hook_code) = run_hook(cmd, "on-ok") {
                return ExitCode::from(hook_code);
            }
        }
    } else {
        if let Some(cmd) = &on_fail {
            let _ = run_hook(cmd, "on-fail");
        }
        if on_ok.is_some() {
            eprintln!("gate-run: --on-ok SKIPPED — gate did not approve (exit {code})");
        }
    }

    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::verdict_name;

    #[test]
    fn verdict_names_match_gate_contract() {
        assert!(verdict_name(0).starts_with("APPROVED"));
        assert!(verdict_name(1).starts_with("REVISE"));
        assert!(verdict_name(2).starts_with("error"));
        assert!(verdict_name(3).starts_with("selftest"));
        assert!(verdict_name(4).starts_with("ownership"));
        assert!(verdict_name(99).contains("fail closed"));
    }
}
