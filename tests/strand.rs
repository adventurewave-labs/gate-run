use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Repo(PathBuf);

impl Repo {
    fn new(name: &str) -> Repo {
        let d = std::env::temp_dir().join(format!("strand-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Repo(d)
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git").arg("-C").arg(&self.0).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    // Dated env so the fixture commit is never byte-identical to its merge.
    fn git_env(&self, envs: &[(&str, &str)], args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .envs(envs.iter().copied())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn commit(&self, file: &str, subject: &str) -> String {
        fs::write(self.0.join(file), format!("content of {file}\n")).unwrap();
        self.git(&["add", file]);
        self.git(&["commit", "-q", "-m", subject]);
        self.git(&["rev-parse", "HEAD"]).trim().to_string()
    }

    fn commit_on(&self, branch: &str, file: &str, subject: &str) -> String {
        self.git(&["checkout", "-q", branch]);
        self.commit(file, subject)
    }

    // The real incident shape: mp clicks Merge on the web UI while the agent
    // keeps pushing. GitHub lands a merge commit — and main has ALWAYS moved
    // since the fork (other PRs landed too); fixtures must reproduce that.
    fn web_merge(&self, refname: &str) {
        self.git(&["checkout", "-q", "main"]);
        self.commit("m-moved.txt", "M main moved since the fork");
        self.git_env(
            &[("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")],
            &["merge", "--no-ff", "-m", "Merge pull request (web UI)", refname],
        );
    }

    // Same incident, squash-and-merge button — onto a moved main.
    fn web_squash(&self, refname: &str) {
        self.git(&["checkout", "-q", "main"]);
        self.commit("m-moved.txt", "M main moved since the fork");
        self.git_env(
            &[("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")],
            &["merge", "--squash", refname],
        );
        self.git_env(
            &[("GIT_COMMITTER_DATE", "2005-04-08T22:13:13 +0000")],
            &["commit", "-q", "-m", "lane (#1) (squashed)"],
        );
    }

    fn cherry_pick_dated(&self, refname: &str) {
        self.git_env(
            &[("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")],
            &["cherry-pick", refname],
        );
    }
}

fn strand(repo: &Repo, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_strand"))
        .current_dir(&repo.0)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn init(name: &str) -> Repo {
    let r = Repo::new(name);
    r.git(&["init", "-q", "-b", "main"]);
    r.git(&["config", "user.email", "test@strand"]);
    r.git(&["config", "user.name", "strand-test"]);
    r
}

#[test]
fn detects_merge_commit_residue() {
    let r = init("merge-residue");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "lane-89"]);
    r.commit("b.txt", "B merged from the web UI");
    r.commit("c.txt", "C merged from the web UI");
    r.web_merge("lane-89");
    // the agent is still pushing while mp clicks merge
    r.commit_on("lane-89", "d.txt", "D pushed after the early merge");
    let stranded_sha = r.commit_on("lane-89", "e.txt", "E pushed after the early merge");
    r.git(&["checkout", "-q", "main"]);

    let (code, stdout, stderr) = strand(&r, &["--all"]);
    assert_eq!(
        code, 1,
        "merge-commit early merge must be RESIDUE — got stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("EARLY-MERGE RESIDUE"), "stdout:\n{stdout}");
    assert!(stdout.contains("D pushed after the early merge"), "stdout:\n{stdout}");
    assert!(stdout.contains("E pushed after the early merge"), "stdout:\n{stdout}");
    assert!(stdout.contains(&stranded_sha), "must list the stranded SHA:\n{stdout}");
    assert!(stdout.contains("git cherry-pick"), "must print a rescue:\n{stdout}");
    assert!(stderr.contains("printed, not run"));
}

#[test]
fn detects_squash_merge_residue() {
    let r = init("squash-residue");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "lane-90"]);
    r.commit("b.txt", "B squashed by the web UI");
    r.commit("c.txt", "C squashed by the web UI");
    r.web_squash("lane-90");
    r.commit_on("lane-90", "d.txt", "D stranded after squash merge");
    r.git(&["checkout", "-q", "main"]);

    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 1, "squash early merge must be RESIDUE — stdout:\n{stdout}");
    assert!(stdout.contains("EARLY-MERGE RESIDUE"), "stdout:\n{stdout}");
    assert!(stdout.contains("D stranded after squash merge"), "stdout:\n{stdout}");
    assert!(!stdout.contains("B squashed"), "squashed commits are NOT stranded:\n{stdout}");
}

#[test]
fn detects_squash_residue_when_main_kept_moving() {
    // Gate r2 scenario: several other PRs land on main between the fork and
    // the squash — tree equality with the branch tip can never hold here;
    // only cumulative content containment proves the branch is merged.
    let r = init("squash-moved-main");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "lane-92"]);
    r.commit("b.txt", "B branch work");
    r.git(&["checkout", "-q", "main"]);
    r.commit("m1.txt", "M1 other PR lands");
    r.commit("m2.txt", "M2 another PR lands");
    r.git_env(
        &[("GIT_COMMITTER_DATE", "2005-04-07T22:13:13 +0000")],
        &["merge", "--squash", "lane-92"],
    );
    r.git_env(
        &[("GIT_COMMITTER_DATE", "2005-04-08T22:13:13 +0000")],
        &["commit", "-q", "-m", "lane (#2) (squashed)"],
    );
    r.commit_on("lane-92", "c.txt", "C stranded after the squash onto moved main");
    r.git(&["checkout", "-q", "main"]);

    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 1, "moved-main squash must still be RESIDUE — stdout:\n{stdout}");
    assert!(stdout.contains("EARLY-MERGE RESIDUE"), "stdout:\n{stdout}");
    assert!(stdout.contains("C stranded after the squash"), "stdout:\n{stdout}");
}

#[test]
fn detects_cherry_pick_residue() {
    let r = init("cherry-residue");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "lane-91"]);
    r.commit("b.txt", "B picked across");
    r.commit_on("lane-91", "c.txt", "C stranded after pick");
    r.git(&["checkout", "-q", "main"]);
    r.cherry_pick_dated("lane-91~1");

    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 1, "cherry-pick early merge must be RESIDUE — stdout:\n{stdout}");
    assert!(stdout.contains("C stranded after pick"), "stdout:\n{stdout}");
    assert!(!stdout.contains("B picked across"), "merged commit is not stranded:\n{stdout}");
}

#[test]
fn later_hotfix_pick_is_not_residue() {
    // Order matters (gate r1): only a LATER commit was cherry-picked into
    // base — this is a normal working branch, not an early merge.
    let r = init("hotfix");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "wip-lane"]);
    r.commit("x.txt", "X work in progress");
    r.commit("h.txt", "H urgent hotfix");
    r.git(&["checkout", "-q", "main"]);
    r.cherry_pick_dated("wip-lane");

    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 0, "hotfix pick must not flag residue — stdout:\n{stdout}");
    assert!(stdout.contains("UNMERGED"), "stdout:\n{stdout}");
    assert!(!stdout.contains("RESIDUE"), "stdout:\n{stdout}");
}

#[test]
fn current_branch_is_included_in_all() {
    let r = init("current");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "lane-current"]);
    r.commit("b.txt", "B merged early");
    r.web_merge("lane-current");
    r.commit_on("lane-current", "c.txt", "C stranded while standing here");

    // standing ON the stranded branch — must still be reported
    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 1, "stdout:\n{stdout}");
    assert!(stdout.contains("lane-current"), "current branch must be checked:\n{stdout}");
    assert!(stdout.contains("EARLY-MERGE RESIDUE"), "stdout:\n{stdout}");
}

#[test]
fn fully_merged_branch_is_safe_to_delete() {
    let r = init("merged");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "done-lane"]);
    r.commit("d.txt", "D merged via fast-forward");
    r.git(&["checkout", "-q", "main"]);
    r.git(&["merge", "--ff-only", "-q", "done-lane"]);

    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("MERGED"), "stdout:\n{stdout}");
}

#[test]
fn branch_merged_by_squash_whole_is_not_stranded() {
    // whole branch squash-merged, nothing pushed after — safe to delete
    let r = init("squash-whole");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "picked-lane"]);
    r.commit("p.txt", "P landed as a squash");
    r.web_squash("picked-lane");
    r.git(&["checkout", "-q", "main"]);

    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 0, "fully squash-merged is not stranded — stdout:\n{stdout}");
    assert!(stdout.contains("MERGED"), "stdout:\n{stdout}");
}

#[test]
fn fresh_working_branch_is_unmerged_not_stranded() {
    let r = init("unmerged");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "wip-lane"]);
    r.commit("w.txt", "W work in progress");
    r.git(&["checkout", "-q", "main"]);

    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("UNMERGED"), "stdout:\n{stdout}");
}

#[test]
fn explicit_branch_and_custom_base() {
    let r = init("explicit");
    r.git(&["checkout", "-q", "-b", "trunk"]);
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "feature"]);
    r.commit("f1.txt", "F1 merged early");
    r.commit("f2.txt", "F2 stranded");
    r.git(&["checkout", "-q", "trunk"]);
    r.cherry_pick_dated("feature~1");

    let (code, stdout, _) = strand(&r, &["--base", "trunk", "feature"]);
    assert_eq!(code, 1);
    assert!(stdout.contains("F2 stranded"), "stdout:\n{stdout}");
}

#[test]
fn run_from_subdirectory_stays_correct() {
    // Gate r3: pathspec-based checks used to mis-read from a subdirectory;
    // merge-tree containment must be cwd-independent.
    let r = init("subdir");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "lane-sub"]);
    r.commit("b.txt", "B merged early");
    r.web_squash("lane-sub");
    r.commit_on("lane-sub", "c.txt", "C stranded");
    r.git(&["checkout", "-q", "main"]);
    std::fs::create_dir_all(r.0.join("deep/sub")).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_strand"))
        .current_dir(r.0.join("deep/sub"))
        .args(["--all"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "residue must be found from a subdirectory");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("EARLY-MERGE RESIDUE"), "{stdout}");
    assert!(stdout.contains("C stranded"), "{stdout}");
}

#[test]
fn non_ascii_filename_residue_is_detected() {
    // Gate r3: quotePath-escaped names used to break pathspec matching and
    // fake containment; merge-tree never touches pathspecs.
    let r = init("nonascii");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "lane-cafe"]);
    fs::write(r.0.join("café.txt"), "café\n").unwrap();
    r.git(&["add", "--", "café.txt"]);
    r.git(&["commit", "-q", "-m", "Café merged early"]);
    r.web_squash("lane-cafe");
    r.commit_on("lane-cafe", "d.txt", "D stranded after café squash");
    r.git(&["checkout", "-q", "main"]);

    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 1, "non-ASCII paths must not fake containment — stdout:\n{stdout}");
    assert!(stdout.contains("D stranded after café squash"), "{stdout}");
}

#[test]
fn reedited_lane_path_reads_unmerged_not_false_merged() {
    // Documented conservative limit: base re-edits a lane path AFTER the
    // squash — containment can no longer prove the merge, so the lane reads
    // UNMERGED (exit 0). A miss is acceptable; a false MERGED is not.
    let r = init("reedited");
    r.commit("a.txt", "A base commit");
    r.git(&["checkout", "-q", "-b", "lane-re"]);
    r.commit("b.txt", "B squashed");
    r.web_squash("lane-re");
    fs::write(r.0.join("b.txt"), "b edited on main after the squash\n").unwrap();
    r.git(&["add", "b.txt"]);
    r.git(&["commit", "-q", "-m", "M main re-edits the lane path"]);
    r.commit_on("lane-re", "c.txt", "C stranded");
    r.git(&["checkout", "-q", "main"]);

    let (code, stdout, _) = strand(&r, &["--all"]);
    assert_eq!(code, 0, "conservative miss, not a false MERGED — stdout:\n{stdout}");
    assert!(stdout.contains("UNMERGED"), "{stdout}");
    assert!(!stdout.contains("RESIDUE"), "{stdout}");
}

#[test]
fn not_a_repo_exits_2() {
    let r = Repo::new("norepo"); // initialized dir, never git-init'ed
    let (code, _, stderr) = strand(&r, &["--all"]);
    assert_eq!(code, 2);
    assert!(stderr.contains("not inside a git repository"));
}
