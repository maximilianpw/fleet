//! The git steps of `fleet move`, run against real git in a temporary
//! directory. Skips without git unless `FLEET_REQUIRE_GIT` is set.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use fleet::moving::{parse_key_values, source_repo_script, target_prep_script, SourceRepo};

fn git_available() -> bool {
    match Command::new("git").arg("--version").output() {
        Ok(output) if output.status.success() => true,
        Ok(_) | Err(_) if std::env::var_os("FLEET_REQUIRE_GIT").is_some() => {
            panic!("git is required for this test")
        }
        Ok(_) | Err(_) => {
            eprintln!("skipping: git is unavailable");
            false
        }
    }
}

struct Sandbox {
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir()
            .join("fleet-move-git")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let bin = root.join("bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let tmux = bin.join("tmux");
        fs::write(&tmux, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).unwrap();
        Self { root, home, bin }
    }

    fn tmux(&self) -> String {
        self.bin.join("tmux").display().to_string()
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Dev")
            .env("GIT_AUTHOR_EMAIL", "dev@example.test")
            .env("GIT_COMMITTER_NAME", "Dev")
            .env("GIT_COMMITTER_EMAIL", "dev@example.test");
        command
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let output = self
            .command("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn run_script(&self, script: &str) -> (i32, String) {
        let output = self.command("sh").arg("-c").arg(script).output().unwrap();
        (
            output.status.code().unwrap_or(255),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    }

    fn commit(&self, dir: &Path, file: &str) {
        fs::write(dir.join(file), file).unwrap();
        self.git(dir, &["add", file]);
        self.git(dir, &["commit", "--quiet", "-m", file]);
    }

    fn seed(&self) -> (PathBuf, PathBuf) {
        let bare = self.root.join("origin.git");
        let source = self.home.join("src/app");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        self.git(
            &self.root,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        self.git(
            &self.root,
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                source.to_str().unwrap(),
            ],
        );
        self.git(&source, &["checkout", "--quiet", "-b", "main"]);
        self.commit(&source, "base");
        self.git(&source, &["push", "--quiet", "origin", "main"]);
        self.git(&source, &["checkout", "--quiet", "-b", "topic"]);
        self.commit(&source, "first");
        self.git(
            &source,
            &["push", "--quiet", "-u", "origin", "topic:feature/x"],
        );
        (bare, source)
    }

    fn advance(&self, source: &Path, file: &str) {
        self.commit(source, file);
        self.git(source, &["push", "--quiet", "origin", "HEAD:feature/x"]);
    }

    fn source_facts(&self, source: &Path, bare: &Path) -> SourceRepo {
        let (code, stdout) = self.run_script(&source_repo_script(source.to_str().unwrap()));
        assert_eq!(code, 0, "{stdout}");
        let facts = parse_key_values(&stdout);
        SourceRepo {
            top: facts["top"].clone(),
            branch: facts["branch"].clone(),
            remote: facts["remote"].clone(),
            upstream: facts["upstream"].clone(),
            head: facts["head"].clone(),
            url: bare.display().to_string(),
            home: facts["home"].clone(),
        }
    }

    fn target_dir(&self) -> PathBuf {
        self.root.join("target-home/src/app")
    }

    fn clone_target(&self, bare: &Path) -> PathBuf {
        let target = self.target_dir();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        self.git(
            &self.root,
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                target.to_str().unwrap(),
            ],
        );
        target
    }

    fn prep(&self, repo: &SourceRepo) -> (i32, String) {
        self.run_script(&target_prep_script(
            repo,
            self.target_dir().to_str().unwrap(),
            &self.tmux(),
            "agents",
        ))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn source_script_reports_facts_for_a_clean_pushed_branch() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("facts");
    let (_, source) = sandbox.seed();
    let (code, stdout) = sandbox.run_script(&source_repo_script(source.to_str().unwrap()));
    assert_eq!(code, 0, "{stdout}");
    let facts = parse_key_values(&stdout);
    assert_eq!(facts["branch"], "topic");
    assert_eq!(facts["remote"], "origin");
    assert_eq!(facts["upstream"], "feature/x");
    assert_eq!(facts["top"], source.to_str().unwrap());
    assert_eq!(facts["head"].len(), 40);
    assert!(facts["head"].chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn source_script_exit_codes() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("source-codes");
    let (_, source) = sandbox.seed();
    let exit = |dir: &Path| {
        sandbox
            .run_script(&source_repo_script(dir.to_str().unwrap()))
            .0
    };

    fs::write(source.join("stray"), "x").unwrap();
    assert_eq!(exit(&source), 12);
    fs::remove_file(source.join("stray")).unwrap();

    sandbox.commit(&source, "unpushed");
    assert_eq!(exit(&source), 15);
    sandbox.git(&source, &["reset", "--quiet", "--hard", "@{u}"]);
    assert_eq!(exit(&source), 0);

    sandbox.git(&source, &["checkout", "--quiet", "-b", "lonely"]);
    assert_eq!(exit(&source), 14);

    sandbox.git(&source, &["checkout", "--quiet", "--detach"]);
    assert_eq!(exit(&source), 13);

    let plain = sandbox.root.join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert_eq!(exit(&plain), 11);
    assert_eq!(exit(&sandbox.root.join("missing")), 10);
}

#[test]
fn target_prep_clones_and_tracks_the_differently_named_upstream() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("clone");
    let (bare, source) = sandbox.seed();
    let repo = sandbox.source_facts(&source, &bare);
    let (code, stdout) = sandbox.prep(&repo);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.trim_end().ends_with("ok"), "{stdout}");
    let target = sandbox.target_dir();
    assert_eq!(sandbox.git(&target, &["rev-parse", "HEAD"]), repo.head);
    assert_eq!(
        sandbox.git(&target, &["rev-parse", "--abbrev-ref", "topic@{u}"]),
        "origin/feature/x"
    );
}

#[test]
fn target_prep_fast_forwards_an_existing_clean_checkout() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("fast-forward");
    let (bare, source) = sandbox.seed();
    let target = sandbox.clone_target(&bare);
    sandbox.git(
        &target,
        &["checkout", "--quiet", "-b", "topic", "origin/feature/x"],
    );
    let older = sandbox.git(&target, &["rev-parse", "HEAD"]);
    sandbox.advance(&source, "second");
    let repo = sandbox.source_facts(&source, &bare);
    assert_ne!(repo.head, older);
    let (code, stdout) = sandbox.prep(&repo);
    assert_eq!(code, 0, "{stdout}");
    assert_eq!(sandbox.git(&target, &["rev-parse", "HEAD"]), repo.head);
}

#[test]
fn target_prep_refuses_a_dirty_target() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("dirty");
    let (bare, source) = sandbox.seed();
    let target = sandbox.clone_target(&bare);
    fs::write(target.join("stray"), "x").unwrap();
    let before = sandbox.git(&target, &["symbolic-ref", "--short", "HEAD"]);
    let repo = sandbox.source_facts(&source, &bare);
    let (code, stdout) = sandbox.prep(&repo);
    assert_eq!(code, 25, "{stdout}");
    assert_eq!(
        sandbox.git(&target, &["symbolic-ref", "--short", "HEAD"]),
        before
    );
}

#[test]
fn target_prep_refuses_a_diverged_target_branch() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("diverged");
    let (bare, source) = sandbox.seed();
    let target = sandbox.clone_target(&bare);
    sandbox.git(
        &target,
        &["checkout", "--quiet", "-b", "topic", "origin/feature/x"],
    );
    sandbox.commit(&target, "local-only");
    sandbox.git(&target, &["checkout", "--quiet", "main"]);
    sandbox.advance(&source, "second");
    let repo = sandbox.source_facts(&source, &bare);
    let (code, stdout) = sandbox.prep(&repo);
    assert_eq!(code, 30, "{stdout}");
    // plan 004 changes this
    assert_eq!(
        sandbox.git(&target, &["symbolic-ref", "--short", "HEAD"]),
        "topic"
    );
}

#[test]
fn target_prep_refuses_when_the_commit_is_missing() {
    if !git_available() {
        return;
    }
    let sandbox = Sandbox::new("missing-commit");
    let (bare, source) = sandbox.seed();
    let mut repo = sandbox.source_facts(&source, &bare);
    repo.head = "0123456789abcdef0123456789abcdef01234567".to_string();
    let (code, stdout) = sandbox.prep(&repo);
    assert_eq!(code, 31, "{stdout}");
    // plan 004 changes this
    assert_eq!(
        sandbox.git(&sandbox.target_dir(), &["symbolic-ref", "--short", "HEAD"]),
        "topic"
    );
}
