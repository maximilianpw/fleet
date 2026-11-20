//! `fleet move HOST SESSION TARGET`: hand an idle agent session to another
//! machine with its conversation.
//!
//! Every check runs before anything is started on the target, and the source
//! session is ended only after the target session is confirmed running. A
//! failure at any step leaves the source untouched.
//!
//! Order: find the agent in SESSION on HOST; require a clean, pushed git
//! branch there; prepare the same home-relative checkout on TARGET (clone if
//! missing, fast-forward only) and require it to contain the source commit;
//! copy the transcript through this machine;
//! start the agent's resume command in a detached tmux session on TARGET;
//! then end the source session unless `--keep`.

use std::collections::BTreeMap;
use std::io::Write;

use thiserror::Error;

use crate::agents::{
    clear_records, local_agents, now_secs, parse_agent_records, resume_command, tmux_panes,
    AgentEnv, AgentRecord, AgentState,
};
use crate::config::{is_safe_session_name, FleetConfig, DEFAULT_TMUX_COMMAND};
use crate::remote::{
    sh_quote, validate_script_value, Endpoint, QueryOutput, RemoteError, Runner, REMOTE_PATH_SETUP,
    TRANSFER_TIMEOUT,
};

#[derive(Debug, Error)]
pub enum MoveError {
    #[error("fleet: unknown Fleet host: {0}")]
    UnknownHost(String),
    #[error("fleet: move needs two different hosts")]
    SameHost,
    #[error("fleet: session names may only contain A-Z, a-z, 0-9, _, ., and -")]
    InvalidSession,
    #[error("fleet: no agent has reported from session '{session}' on {host}; agents report through `fleet hook`")]
    NoAgent { host: String, session: String },
    #[error("fleet: session '{session}' on {host} has {count} agents; move needs exactly one")]
    MultipleAgents {
        host: String,
        session: String,
        count: usize,
    },
    #[error("fleet: the agent in '{session}' is {state}; wait for it to finish or pass --force")]
    Busy { session: String, state: String },
    #[error("fleet: agent '{0}' has no known resume command; report one with `fleet hook set --resume-command`")]
    NoResumeCommand(String),
    #[error("fleet: the agent record has no {0}; it cannot be moved")]
    MissingField(&'static str),
    #[error("fleet: could not read agent state on {host}: {reason}")]
    AgentState { host: String, reason: String },
    #[error("fleet: {session} now runs on {target}, but it was not ended on {source_host}: {reason}; end it there yourself once you have checked it, then attach with: fleet ssh {target} {session}")]
    BothRunning {
        session: String,
        source_host: String,
        target: String,
        reason: String,
    },
    #[error("fleet: {host}: {reason}")]
    Step { host: String, reason: String },
    #[error(transparent)]
    Remote(#[from] RemoteError),
    #[error("fleet: failed to write output: {0}")]
    Output(#[from] std::io::Error),
}

impl MoveError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::UnknownHost(_) | Self::SameHost | Self::InvalidSession => 2,
            Self::Remote(error) => error.exit_code(),
            Self::BothRunning { .. } => 3,
            _ => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRequest {
    pub host: String,
    pub session: String,
    pub target: String,
    pub keep: bool,
    pub force: bool,
}

struct Side {
    name: String,
    endpoint: Endpoint,
    tmux: String,
    is_local: bool,
}

fn side(config: &FleetConfig, token: &str) -> Result<Side, MoveError> {
    let resolved = config
        .resolve(token)
        .ok_or_else(|| MoveError::UnknownHost(token.to_string()))?;
    let (endpoint, tmux) = if resolved.is_local {
        (Endpoint::Local, DEFAULT_TMUX_COMMAND.to_string())
    } else {
        (
            Endpoint::Ssh {
                target: resolved.ssh_target.clone(),
            },
            resolved.tmux_command.clone(),
        )
    };
    Ok(Side {
        name: resolved.canonical,
        endpoint,
        tmux,
        is_local: resolved.is_local,
    })
}

/// Facts about the source checkout, printed as `key=value` lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceRepo {
    pub top: String,
    pub branch: String,
    pub remote: String,
    /// The upstream's branch name on `remote`, which may differ from `branch`.
    pub upstream: String,
    pub head: String,
    pub url: String,
    pub home: String,
}

pub fn source_repo_script(cwd: &str) -> String {
    format!(
        ": fleet-git-source; {REMOTE_PATH_SETUP}; \
cd {cwd} || exit 10; \
top=$(git rev-parse --show-toplevel 2>/dev/null) || exit 11; \
[ -z \"$(git status --porcelain)\" ] || exit 12; \
branch=$(git symbolic-ref --short -q HEAD) || exit 13; \
git rev-parse -q --verify @{{u}} >/dev/null 2>&1 || exit 14; \
remote=$(git config --get \"branch.$branch.remote\") || exit 14; \
merge=$(git config --get \"branch.$branch.merge\") || exit 14; \
url=$(git remote get-url \"$remote\" 2>/dev/null) || exit 14; \
head=$(git rev-parse HEAD); \
[ \"$head\" = \"$(git rev-parse @{{u}})\" ] || exit 15; \
echo \"top=$top\"; echo \"branch=$branch\"; echo \"remote=$remote\"; \
echo \"upstream=${{merge#refs/heads/}}\"; echo \"head=$head\"; \
echo \"url=$url\"; echo \"home=$HOME\"",
        cwd = sh_quote(cwd)
    )
}

fn source_failure(code: i32) -> &'static str {
    match code {
        10 => "the agent's working directory no longer exists",
        11 => "the agent's working directory is not a git repository",
        12 => "the checkout has uncommitted or untracked changes; commit and push first",
        13 => "the checkout is not on a branch",
        14 => "the branch has no upstream on a remote; push it first",
        15 => "the branch is not pushed (HEAD differs from its upstream); push first",
        _ => "checking the source checkout failed",
    }
}

pub fn parse_key_values(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.to_string()))
        .collect()
}

pub fn target_home_script() -> String {
    ": fleet-target-home; echo \"home=$HOME\"".to_string()
}

pub fn target_prep_script(repo: &SourceRepo, dir: &str, tmux: &str, session: &str) -> String {
    let parent = dir
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or(".");
    format!(
        ": fleet-target-prep; {REMOTE_PATH_SETUP}; \
command -v git >/dev/null 2>&1 || exit 21; \
command -v {tmux} >/dev/null 2>&1 || exit 22; \
{tmux} has-session -t ={session} 2>/dev/null && exit 23; \
if [ -e {dir}/.git ]; then \
cd {dir} || exit 24; \
[ -z \"$(git status --porcelain)\" ] || exit 25; \
git fetch --quiet {remote} || exit 26; \
else \
mkdir -p {parent} || exit 27; \
git clone --quiet -o {remote} -- {url} {dir} || exit 28; \
cd {dir} || exit 24; \
fi; \
git merge-base --is-ancestor {head} {tracking} || exit 31; \
if git rev-parse -q --verify refs/heads/{branch} >/dev/null; then \
git merge-base --is-ancestor refs/heads/{branch} {tracking} || git merge-base --is-ancestor {tracking} refs/heads/{branch} || exit 30; \
git checkout --quiet {branch} || exit 29; \
else git checkout --quiet -b {branch} --track {tracking} || exit 29; fi; \
git merge --ff-only --quiet {tracking} || exit 30; \
git merge-base --is-ancestor {head} HEAD || exit 31; \
echo ok",
        dir = sh_quote(dir),
        parent = sh_quote(parent),
        url = sh_quote(&repo.url),
        remote = sh_quote(&repo.remote),
        branch = sh_quote(&repo.branch),
        tracking = sh_quote(&format!("{}/{}", repo.remote, repo.upstream)),
        head = sh_quote(&repo.head),
        session = sh_quote(session),
    )
}

fn target_failure(code: i32) -> &'static str {
    match code {
        21 => "git is not installed",
        22 => "tmux is not installed",
        23 => "a tmux session with that name already exists",
        24 => "cannot enter the target checkout",
        25 => "the target checkout has uncommitted changes",
        26 => "git fetch failed in the target checkout",
        27 => "cannot create the target directory",
        28 => "git clone failed",
        29 => "cannot check out the branch",
        30 => "the target branch cannot fast-forward to the upstream (it has diverged)",
        31 => "the target branch does not contain the commit the source is on",
        _ => "preparing the target checkout failed",
    }
}

pub fn start_script(tmux: &str, session: &str, cwd: &str, command: &str) -> String {
    format!(
        ": fleet-start; {REMOTE_PATH_SETUP}; \
{tmux} new-session -d -s {session} -c {cwd} {command} || exit 40; \
sleep 2; \
{tmux} has-session -t ={session} 2>/dev/null || exit 41; echo ok",
        session = sh_quote(session),
        cwd = sh_quote(cwd),
        command = sh_quote(command),
    )
}

pub fn stop_script(tmux: &str, session: &str) -> String {
    format!(
        ": fleet-stop; {REMOTE_PATH_SETUP}; {tmux} kill-session -t ={session}",
        session = sh_quote(session)
    )
}

/// Claude Code stores a project's sessions under its cwd with every
/// non-alphanumeric character replaced by `-`.
pub fn claude_project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Where the transcript goes on the target.
pub fn target_transcript_path(
    record: &AgentRecord,
    transcript: &str,
    source_home: &str,
    target_home: &str,
    target_cwd: &str,
) -> Option<String> {
    let file = transcript.rsplit('/').next()?;
    if matches!(record.agent.as_str(), "claude" | "claude-code") {
        return Some(format!(
            "{target_home}/.claude/projects/{}/{file}",
            claude_project_slug(target_cwd)
        ));
    }
    let relative = transcript.strip_prefix(&format!("{source_home}/"))?;
    Some(format!("{target_home}/{relative}"))
}

/// Map a path under one home directory to the same place under another.
pub fn rehome(path: &str, from_home: &str, to_home: &str) -> Option<String> {
    if path == from_home {
        return Some(to_home.to_string());
    }
    path.strip_prefix(&format!("{from_home}/"))
        .map(|relative| format!("{to_home}/{relative}"))
}

fn step_error(side: &Side, output: &QueryOutput, reason: &str) -> MoveError {
    let detail = output.stderr.trim();
    let reason = if output.timed_out {
        format!("{reason} (timed out)")
    } else if detail.is_empty() {
        reason.to_string()
    } else {
        format!("{reason}: {detail}")
    };
    MoveError::Step {
        host: side.name.clone(),
        reason,
    }
}

fn agents_on(
    side: &Side,
    runner: &Runner,
    agent_env: &AgentEnv,
) -> Result<Vec<AgentRecord>, MoveError> {
    if side.is_local {
        let dir = agent_env
            .state_dir()
            .map_err(|error| MoveError::AgentState {
                host: side.name.clone(),
                reason: error.to_string(),
            })?;
        return local_agents(&dir, &tmux_panes(), now_secs()).map_err(|error| {
            MoveError::AgentState {
                host: side.name.clone(),
                reason: error.to_string(),
            }
        });
    }
    let output = runner.run(
        &side.endpoint,
        &format!(": fleet-agents; {REMOTE_PATH_SETUP}; fleet agents --local --json"),
    );
    if !output.success() {
        return Err(MoveError::AgentState {
            host: side.name.clone(),
            reason: if output.code == 127 {
                "fleet is not on the remote PATH".into()
            } else {
                output.stderr.trim().to_string()
            },
        });
    }
    parse_agent_records(&output.stdout).ok_or_else(|| MoveError::AgentState {
        host: side.name.clone(),
        reason: "`fleet agents --local --json` did not print a list of records".into(),
    })
}

/// Why the source agent must not be ended: it reported again after the
/// transcript was read, it is still working (a forced move), or its state
/// can no longer be read.
fn source_changed(
    source: &Side,
    record: &AgentRecord,
    session: &str,
    runner: &Runner,
    agent_env: &AgentEnv,
) -> Option<String> {
    let current = match agents_on(source, runner, agent_env) {
        Ok(agents) => agents.into_iter().find(|agent| agent.key == record.key),
        Err(error) => return Some(format!("could not re-check the source agent ({error})")),
    };
    match current {
        Some(agent) if agent.tmux_session.as_deref() != Some(session) => Some(format!(
            "the source agent's pane is no longer in session {session}"
        )),
        Some(agent) if agent.updated_at != record.updated_at => {
            Some("the source agent reported activity after its transcript was copied".to_string())
        }
        Some(agent) if agent.state.is_busy() => {
            Some(format!("the source agent is still {}", agent.state.label()))
        }
        _ => None,
    }
}

pub fn run_move(
    config: &FleetConfig,
    request: &MoveRequest,
    runner: &Runner,
    agent_env: &AgentEnv,
    out: &mut dyn Write,
) -> Result<(), MoveError> {
    if !is_safe_session_name(&request.session) {
        return Err(MoveError::InvalidSession);
    }
    let source = side(config, &request.host)?;
    let target = side(config, &request.target)?;
    if source.name == target.name {
        return Err(MoveError::SameHost);
    }

    let agents: Vec<AgentRecord> = agents_on(&source, runner, agent_env)?
        .into_iter()
        .filter(|agent| {
            agent.tmux_session.as_deref() == Some(request.session.as_str())
                && agent.state != AgentState::Gone
        })
        .collect();
    let record = match agents.as_slice() {
        [] => {
            return Err(MoveError::NoAgent {
                host: source.name.clone(),
                session: request.session.clone(),
            })
        }
        [record] => record.clone(),
        many => {
            return Err(MoveError::MultipleAgents {
                host: source.name.clone(),
                session: request.session.clone(),
                count: many.len(),
            })
        }
    };
    if record.state.is_busy() && !request.force {
        return Err(MoveError::Busy {
            session: request.session.clone(),
            state: record.state.label().to_string(),
        });
    }
    let resume =
        resume_command(&record).ok_or_else(|| MoveError::NoResumeCommand(record.agent.clone()))?;
    let cwd = record.cwd.clone().ok_or(MoveError::MissingField("cwd"))?;
    // A recorded resume command replaces only how the agent restarts; any
    // known transcript still has to reach the target.
    if record.resume_command.is_none() && record.transcript.is_none() {
        return Err(MoveError::MissingField("transcript path"));
    }
    for value in [&cwd, &resume]
        .into_iter()
        .chain(record.transcript.as_ref())
    {
        validate_script_value(value)?;
    }
    writeln!(
        out,
        "fleet: moving {} ({}) from {} to {}",
        request.session, record.agent, source.name, target.name
    )?;

    let output = runner.run(&source.endpoint, &source_repo_script(&cwd));
    if !output.success() {
        return Err(step_error(&source, &output, source_failure(output.code)));
    }
    let facts = parse_key_values(&output.stdout);
    let fact = |key: &str| facts.get(key).cloned().unwrap_or_default();
    let repo = SourceRepo {
        top: fact("top"),
        branch: fact("branch"),
        remote: fact("remote"),
        upstream: fact("upstream"),
        head: fact("head"),
        url: fact("url"),
        home: fact("home"),
    };
    let reported = [
        &repo.top,
        &repo.branch,
        &repo.remote,
        &repo.upstream,
        &repo.head,
        &repo.url,
        &repo.home,
    ];
    if reported.iter().any(|value| value.is_empty()) {
        return Err(step_error(
            &source,
            &output,
            "the source checkout did not report its branch, upstream, commit, and home",
        ));
    }
    for value in reported {
        validate_script_value(value)?;
    }
    // These become git arguments on the target, where a leading `-` would be
    // read as an option.
    let option_like = [&repo.branch, &repo.remote, &repo.upstream, &repo.url]
        .into_iter()
        .find(|value| value.starts_with('-'));
    if let Some(value) = option_like {
        return Err(MoveError::Step {
            host: source.name.clone(),
            reason: format!("the source checkout reported an option-like git value: {value}"),
        });
    }
    if !repo.head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(MoveError::Step {
            host: source.name.clone(),
            reason: format!(
                "the source checkout reported an invalid commit: {}",
                repo.head
            ),
        });
    }

    let output = runner.run(&target.endpoint, &target_home_script());
    let target_home = parse_key_values(&output.stdout)
        .remove("home")
        .filter(|home| output.success() && !home.is_empty())
        .ok_or_else(|| step_error(&target, &output, "cannot read the home directory"))?;
    validate_script_value(&target_home)?;
    let target_top =
        rehome(&repo.top, &repo.home, &target_home).ok_or_else(|| MoveError::Step {
            host: source.name.clone(),
            reason: format!(
                "the checkout {} is outside the home directory, so its place on {} is unknown",
                repo.top, target.name
            ),
        })?;
    let target_cwd = rehome(&cwd, &repo.home, &target_home).unwrap_or_else(|| target_top.clone());

    writeln!(
        out,
        "fleet: preparing {} on {} at {target_top}",
        repo.branch, target.name
    )?;
    // A clone or fetch can take minutes; the query default is 20 seconds.
    let output = runner.clone().with_timeout(TRANSFER_TIMEOUT).run(
        &target.endpoint,
        &target_prep_script(&repo, &target_top, &target.tmux, &request.session),
    );
    if !output.success() {
        return Err(step_error(&target, &output, target_failure(output.code)));
    }

    if let Some(transcript) = record.transcript.as_deref() {
        let destination =
            target_transcript_path(&record, transcript, &repo.home, &target_home, &target_cwd)
                .ok_or_else(|| MoveError::Step {
                    host: source.name.clone(),
                    reason: format!("the transcript {transcript} is outside the home directory"),
                })?;
        let dest_dir = destination
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_else(|| ".".into());
        writeln!(out, "fleet: copying the transcript to {destination}")?;
        // A failed read still ends the writer's input cleanly, so the writer
        // cannot tell a complete transcript from a cut-off one. Only this
        // side can, and it puts the file in place after both succeeded.
        let partial = format!("{destination}.fleet-move.tmp");
        let copied = runner.pipe(
            &source.endpoint,
            &format!(": fleet-transcript-read; cat -- {}", sh_quote(transcript)),
            &target.endpoint,
            &format!(
                ": fleet-transcript-write; mkdir -p {} && cat > {}",
                sh_quote(&dest_dir),
                sh_quote(&partial)
            ),
            TRANSFER_TIMEOUT,
        );
        if let Err(error) = copied {
            runner.run(
                &target.endpoint,
                &format!(
                    ": fleet-transcript-discard; rm -f -- {}",
                    sh_quote(&partial)
                ),
            );
            return Err(error.into());
        }
        let output = runner.run(
            &target.endpoint,
            &format!(
                ": fleet-transcript-commit; mv -f -- {} {}",
                sh_quote(&partial),
                sh_quote(&destination)
            ),
        );
        if !output.success() {
            return Err(step_error(
                &target,
                &output,
                "cannot put the copied transcript in place",
            ));
        }
    }

    writeln!(
        out,
        "fleet: starting `{resume}` in tmux session {} on {}",
        request.session, target.name
    )?;
    let output = runner.run(
        &target.endpoint,
        &start_script(&target.tmux, &request.session, &target_cwd, &resume),
    );
    if !output.success() {
        let reason = match output.code {
            41 if !output.timed_out => {
                "the resumed session exited right away; the source session was left running"
                    .to_string()
            }
            40 if !output.timed_out => {
                "starting the tmux session failed; the source session was left running".to_string()
            }
            _ => format!(
                "could not confirm the session started; the source session was left running, and {session} may already be running on {target} (check with: fleet ssh {target} {session})",
                session = request.session,
                target = target.name
            ),
        };
        return Err(step_error(&target, &output, &reason));
    }

    if request.keep {
        writeln!(out, "fleet: kept the source session on {}", source.name)?;
    } else {
        let both_running = |reason: String| MoveError::BothRunning {
            session: request.session.clone(),
            source_host: source.name.clone(),
            target: target.name.clone(),
            reason,
        };
        if let Some(reason) = source_changed(&source, &record, &request.session, runner, agent_env)
        {
            return Err(both_running(reason));
        }
        let output = runner.run(
            &source.endpoint,
            &stop_script(&source.tmux, &request.session),
        );
        if !output.success() {
            let detail = output.stderr.trim();
            return Err(both_running(if detail.is_empty() {
                "could not end the source session".to_string()
            } else {
                format!("could not end the source session: {detail}")
            }));
        }
        if source.is_local {
            if let (Ok(dir), Some(id)) = (agent_env.state_dir(), record.session_id.as_deref()) {
                let _ = clear_records(&dir, None, Some(id));
            }
        }
    }
    writeln!(
        out,
        "fleet: moved {} to {}; attach with: fleet ssh {} {}",
        request.session, target.name, target.name, request.session
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(agent: &str) -> AgentRecord {
        AgentRecord {
            version: 1,
            key: "pane-1".into(),
            agent: agent.into(),
            state: AgentState::Done,
            task: None,
            request: None,
            message: None,
            session_id: Some("abc".into()),
            transcript: None,
            resume_command: None,
            cwd: None,
            tmux_session: Some("main".into()),
            tmux_pane: Some("%1".into()),
            tmux_server: None,
            updated_at: 0,
        }
    }

    #[test]
    fn claude_slug_replaces_non_alphanumerics() {
        assert_eq!(
            claude_project_slug("/Users/dev/Local/my.repo"),
            "-Users-dev-Local-my-repo"
        );
    }

    #[test]
    fn rehome_maps_paths_between_homes() {
        assert_eq!(
            rehome("/Users/dev/Local/app", "/Users/dev", "/home/dev").as_deref(),
            Some("/home/dev/Local/app")
        );
        assert_eq!(rehome("/srv/app", "/Users/dev", "/home/dev"), None);
        assert_eq!(rehome("/Users/devx/app", "/Users/dev", "/home/dev"), None);
    }

    #[test]
    fn transcript_destinations() {
        assert_eq!(
            target_transcript_path(
                &record("claude"),
                "/Users/dev/.claude/projects/-Users-dev-Local-app/abc.jsonl",
                "/Users/dev",
                "/home/dev",
                "/home/dev/Local/app",
            )
            .as_deref(),
            Some("/home/dev/.claude/projects/-home-dev-Local-app/abc.jsonl")
        );
        assert_eq!(
            target_transcript_path(
                &record("codex"),
                "/Users/dev/.codex/sessions/2026/10/09/rollout-abc.jsonl",
                "/Users/dev",
                "/home/dev",
                "/home/dev/Local/app",
            )
            .as_deref(),
            Some("/home/dev/.codex/sessions/2026/10/09/rollout-abc.jsonl")
        );
        assert_eq!(
            target_transcript_path(
                &record("pi"),
                "/tmp/t.jsonl",
                "/Users/dev",
                "/home/dev",
                "/x"
            ),
            None
        );
    }

    #[test]
    fn scripts_quote_values_and_have_no_backslashes() {
        let repo = SourceRepo {
            top: "/Users/dev/Local/it's".into(),
            branch: "topic".into(),
            remote: "origin".into(),
            upstream: "feature/x".into(),
            head: "0123abcd".into(),
            url: "git@example.test:me/app.git".into(),
            home: "/Users/dev".into(),
        };
        let prep = target_prep_script(&repo, "/home/dev/Local/it's", "tmux", "main");
        assert!(prep.contains("'/home/dev/Local/it'\"'\"'s'"));
        assert!(prep.contains("--track 'origin/feature/x'"), "{prep}");
        assert!(prep.contains("--is-ancestor '0123abcd' HEAD"), "{prep}");
        let first_ancestry = prep.find("--is-ancestor").unwrap();
        let first_checkout = prep.find("git checkout").unwrap();
        assert!(first_ancestry < first_checkout, "{prep}");
        for script in [
            prep,
            source_repo_script("/Users/dev/app"),
            start_script("tmux", "main", "/home/dev/app", "claude --resume abc"),
            stop_script("tmux", "main"),
            target_home_script(),
        ] {
            assert!(!script.contains('\\'), "{script}");
        }
    }

    #[test]
    fn key_values_parse() {
        let facts = parse_key_values("top=/a\nbranch=main\nurl=git@x:y=z\nnoise\n");
        assert_eq!(facts["url"], "git@x:y=z");
        assert_eq!(facts.len(), 3);
    }
}
