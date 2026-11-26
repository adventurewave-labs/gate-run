//! strand — find commits stranded on local branches by early merges. Read-only.
//!
//! Signature of the incident class (3× and counting): a PR is merged from the
//! web UI while later commits are still being pushed to the branch — those
//! commits become unreachable from main and invisible to every dashboard.
//! strand lists them and prints the rescue command; it never touches the repo.
//!
//! Detection is content-based, not patch-id-only (gates r1+r2 on PR #93): a
//! web-UI merge lands as a merge commit or a squash, neither preserves the
//! branch's individual patch-ids, and the squash's tree only equals the
//! branch's when main stood still since the fork. The sound evidence that
//! "base contains the branch up to commit P" is cumulative containment:
//! every path changed since the fork has the same content in base as at P.
//! A merge-committed P is additionally proven by reachability. Patch-ids are
//! only a fallback for the fully-merged-but-not-by-sha case.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "\
strand — find commits stranded on branches by early merges (read-only).

USAGE:
  strand [--base REF] BRANCH...     check specific branches
  strand --all [--base REF]         check every local branch except base

Classifies each branch:
  MERGED                tip is an ancestor of base — safe to delete
  MERGED (patch-equiv)  every branch commit is in base by tree or patch
  EARLY-MERGE RESIDUE   base contains the branch up to some commit P
                        (merge-commit, squash, or cherry-picked), but commits
                        after P are not — the incident signature
  UNMERGED              nothing proves any of the branch is in base — a normal
                        working branch (a later commit hot-picked into base
                        does NOT count as partially merged; order matters)

For EARLY-MERGE RESIDUE, stranded commits are listed oldest-first with a
suggested cherry-pick rescue (printed, never executed).

Exit: 0 = nothing stranded · 1 = stranded commits found · 2 = git or usage error
";

// Per-branch analysis is spawn-per-commit; branches beyond this are refused
// rather than silently slow.
const MAX_BRANCH_COMMITS: usize = 500;

#[derive(Clone)]
struct Commit {
    sha: String,
    subject: String,
}

enum Class {
    Merged,
    MergedByPatch,
    EarlyMergeResidue(Vec<Commit>), // stranded commits, oldest first
    Unmerged(usize),                // count of branch commits not in base
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<bool, String> {
    // for `merge-base --is-ancestor`: exit 0 = true, exit 1 = false
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

fn patch_id(repo: &Path, sha: &str) -> Result<Option<String>, String> {
    let diff = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff-tree", "-p", sha])
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !diff.status.success() {
        return Err(String::from_utf8_lossy(&diff.stderr).trim().to_string());
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["patch-id", "--stable"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git patch-id: {e}"))?;
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&diff.stdout)
        .map_err(|e| format!("patch-id stdin failed: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("patch-id failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git patch-id exited {} — refusing to classify on a failed probe",
            out.status
        ));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let id = line.split_whitespace().next().map(str::to_string);
    Ok(id)
}

fn classify(repo: &Path, base: &str, branch: &str) -> Result<Class, String> {
    if git_ok(repo, &["merge-base", "--is-ancestor", branch, base])? {
        return Ok(Class::Merged);
    }
    let fork = git(repo, &["merge-base", base, branch])?
        .trim()
        .to_string();

    // Branch's own commits: first-parent chain of the branch minus base's
    // first-parent chain (sha-level). Reachability would hide exactly the
    // commits a web-UI merge brought into base (merge-commit case).
    let base_fp: HashSet<String> = git(repo, &["rev-list", "--first-parent", base])?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    // oldest-first own commits with subjects, from one log call
    let mut own: Vec<Commit> = Vec::new();
    for line in git(repo, &["log", "--first-parent", "--format=%H %s", branch])?.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (sha, subject) = match line.split_once(' ') {
            Some(p) => p,
            None => continue,
        };
        if !base_fp.contains(sha) {
            own.push(Commit { sha: sha.to_string(), subject: subject.to_string() });
        }
    }
    own.reverse(); // log is newest-first; work oldest-first
    if own.len() > MAX_BRANCH_COMMITS {
        return Err(format!(
            "branch has {} own commits (cap {MAX_BRANCH_COMMITS}) — refusing slow path",
            own.len()
        ));
    }

    // Cumulative content containment (gate r2 on PR #93): P's work is in base
    // iff every path changed since the fork has the SAME content in base as at
    // P — `git diff --quiet P base -- <paths>` empty. This holds even when
    // base moved after the fork (squash lands on a newer main), which
    // tree-equality and per-commit patch-ids both miss. Measured from the
    // fork, so a lone later hot-pick cannot fake partial containment.
    let contains_up_to = |p: &str| -> Result<bool, String> {
        if git_ok(repo, &["merge-base", "--is-ancestor", p, base])? {
            return Ok(true); // merge-commit case: P itself is reachable
        }
        // -z: NUL-delimited. --name-only's default output QUOTES filenames with
        // tabs/newlines/non-ASCII; passing a quoted string back as a pathspec
        // matches nothing, and `diff --quiet` then succeeds on UNMERGED work.
        // --no-renames: with rename detection ON, a rename supplies only the
        // NEW side; if base kept the old file too, containment would pass on
        // unmerged work. Both sides enumerate as separate paths.
        let diff_out = git(repo, &["diff", "-z", "--no-renames", "--name-only", &fork, p])?;
        let paths: Vec<&str> = diff_out
            .split('\0')
            .filter(|l| !l.is_empty())
            .collect();
        if paths.is_empty() {
            return Ok(false);
        }
        // raw-byte filenames decode lossily (U+FFFD); a phantom path could
        // diff empty and read unmerged work as merged. Refuse to classify.
        if paths.iter().any(|p| p.contains('\u{FFFD}')) {
            return Err("non-UTF-8 filename in the changed set — containment unprovable, refusing (fail closed)".into());
        }
        let mut args = vec!["diff", "--quiet", p, base, "--"];
        args.extend(paths);
        let out = Command::new("git")
            .env("GIT_LITERAL_PATHSPECS", "1") // odd names are names, never globs
            .arg("-C")
            // the TRUE repo root: pathspecs are repo-root-relative, and
            // canonicalize(".") would anchor at the SUBdirectory. Ask git.
            .arg(git(repo, &["rev-parse", "--show-toplevel"])?.trim().to_string())
            .args(&args)
            .output()
            .map_err(|e| format!("cannot run git: {e}"))?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        }
    };

    // merged_up_to = NEWEST own commit P contained in base (scan newest-first,
    // stop at the first hit). Order matters: a LATER commit hot-picked into
    // base must not make the branch look partially merged.
    let mut merged_up_to: Option<usize> = None; // index into `own`
    for (i, c) in own.iter().enumerate().rev() {
        if contains_up_to(&c.sha)? {
            merged_up_to = Some(i);
            break;
        }
    }

    // fallback for the fully-merged-but-not-by-sha case: every own commit
    // individually in base by containment or patch-id. Patch-ids are computed
    // over the fork..base window only (bounded), and merge/root commits yield
    // no patch (diff-tree -p prints nothing) — they simply never match.
    let all_in_base = |c: &Commit| -> Result<bool, String> {
        if contains_up_to(&c.sha)? {
            return Ok(true);
        }
        match patch_id(repo, &c.sha)? {
            Some(id) => {
                for b in git(repo, &["rev-list", "--no-merges", &format!("{fork}..{base}")])?
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                {
                    if patch_id(repo, b)? == Some(id.clone()) {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            None => Ok(false),
        }
    };

    match merged_up_to {
        Some(idx) => {
            let stranded: Vec<Commit> = own[idx + 1..].to_vec();
            if stranded.is_empty() {
                return Ok(Class::MergedByPatch);
            }
            // content containment stopped matching, but a commit whose patch
            // is already IN base (e.g. cherry-picked, then base evolved the
            // same file further) is NOT residue — check before declaring.
            let mut real: Vec<Commit> = Vec::new();
            for c in &stranded {
                let mut already = false;
                if let Some(id) = patch_id(repo, &c.sha)? {
                    for b in git(repo, &["rev-list", "--no-merges", &format!("{fork}..{base}")])?
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                    {
                        if patch_id(repo, b)? == Some(id.clone()) {
                            already = true;
                            break;
                        }
                    }
                }
                // a commit whose patch-id can't be established stays in the
                // residue list — reporting it is safe, dropping it silently is not
                if !already {
                    real.push(c.clone());
                }
            }
            if real.is_empty() {
                Ok(Class::MergedByPatch)
            } else {
                Ok(Class::EarlyMergeResidue(real))
            }
        }
        None => {
            if own.is_empty() {
                return Ok(Class::Merged);
            }
            let mut every = true;
            for c in &own {
                if !all_in_base(c)? {
                    every = false;
                    break;
                }
            }
            if every {
                Ok(Class::MergedByPatch)
            } else {
                Ok(Class::Unmerged(own.len()))
            }
        }
    }
}

fn local_branches(repo: &Path) -> Result<Vec<String>, String> {
    let out = git(repo, &["for-each-ref", "--format=%(refname:short)", "refs/heads"])?;
    Ok(out.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
}

fn report(base: &str, branch: &str, class: &Class) -> bool {
    match class {
        Class::Merged => {
            println!("{branch:<32} MERGED                (tip is in {base} — safe to delete)");
            false
        }
        Class::MergedByPatch => {
            println!("{branch:<32} MERGED (patch-equiv)  (every commit is in {base} by tree or patch)");
            false
        }
        Class::Unmerged(n) => {
            println!("{branch:<32} UNMERGED              ({n} commit(s) not in {base} — normal working branch)");
            false
        }
        Class::EarlyMergeResidue(stranded) => {
            println!("{branch:<32} EARLY-MERGE RESIDUE   ({0} stranded commit(s))", stranded.len());
            for c in stranded {
                println!("  {branch}  {}  {}", &c.sha[..8.min(c.sha.len())], c.subject);
            }
            let shq = |v: &str| format!("'{}'", v.replace('\'', "'\\''"));
            println!("  rescue (review first, then run by hand):");
            println!("    git checkout -b rescue/{} {}", shq(&branch), shq(base));
            for c in stranded {
                println!("    git cherry-pick {}", c.sha);
            }
            true
        }
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut base = "main".to_string();
    let mut all = false;
    let mut branches: Vec<String> = Vec::new();

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--base" => {
                i += 1;
                match argv.get(i) {
                    Some(v) if !v.is_empty() => base = v.clone(),
                    _ => {
                        eprint!("strand: --base needs a ref\n\n{USAGE}");
                        return ExitCode::from(2);
                    }
                }
            }
            "--all" => all = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::from(0);
            }
            other => branches.push(other.to_string()),
        }
        i += 1;
    }

    let repo = PathBuf::from(".");
    if let Err(e) = git(&repo, &["rev-parse", "--git-dir"]) {
        eprintln!("strand: not inside a git repository ({e})");
        return ExitCode::from(2);
    }
    if let Err(e) = git(&repo, &["rev-parse", "--verify", "-q", &format!("{base}^{{commit}}")]) {
        eprintln!("strand: unknown base ref '{base}' ({e})");
        return ExitCode::from(2);
    }

    if all {
        // every local branch except base itself — INCLUDING the branch we are
        // standing on (gate r1: the current lane is the most likely to carry
        // fresh stranded commits)
        match local_branches(&repo) {
            Ok(list) => {
                branches = list.into_iter().filter(|b| b != &base).collect();
            }
            Err(e) => {
                eprintln!("strand: cannot list branches: {e}");
                return ExitCode::from(2);
            }
        }
    }

    if branches.is_empty() {
        eprint!("strand: no branches to check\n\n{USAGE}");
        return ExitCode::from(2);
    }

    let mut any_stranded = false;
    for branch in &branches {
        match classify(&repo, &base, branch) {
            Ok(class) => any_stranded |= report(&base, branch, &class),
            Err(e) => {
                eprintln!("strand: cannot classify '{branch}' against '{base}': {e}");
                return ExitCode::from(2);
            }
        }
    }

    if any_stranded {
        eprintln!("strand: STRANDED COMMITS FOUND — rescue commands above are printed, not run");
        ExitCode::from(1)
    } else {
        ExitCode::from(0)
    }
}
