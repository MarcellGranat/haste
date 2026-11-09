//! The remote feed against a fake `herdr` executable (a shell script). No real herdr, no SSH.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use haste_core::herdr::AgentStatus;
use haste_herdr::command::{HerdrCommand, Runner};
use haste_herdr::remote::{discover, fetch_snapshot};
use haste_herdr::FeedError;

const SCRIPT: &str = r#"#!/bin/sh
D="$(dirname "$0")"
echo "$*" >> "$D/args.log"
case "$1" in
  machine) cat "$D/machines.json" ;;
  --machine)
    M="$2"
    if [ "$3 $4" = "api snapshot" ]; then
      [ -f "$D/fail-$M" ] && exit 1
      [ -f "$D/hang-$M" ] && { sleep 31; exit 0; }
      cat "$D/snapshot-$M.json"
    fi ;;
  agent) exit 0 ;;
esac
"#;

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("herdr");
        fs::write(&bin, SCRIPT).unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            dir.path().join("machines.json"),
            r#"[{"id":"1","label":"m1","target":"u@h","session":"s","enabled":true,"selected":false},
                {"id":"2","label":"m2","target":"u@h2","session":"s","enabled":false,"selected":false},
                {"id":"3","label":"mbp","target":"u@h3","session":"s","enabled":true,"selected":true}]"#,
        )
        .unwrap();
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn runner(&self, timeout: Duration) -> Runner {
        let mut r = Runner::new(self.path().join("herdr"));
        r.timeout = timeout;
        r
    }

    fn snapshot(&self, machine: &str, protocol: u32, status: &str) {
        let json = format!(
            r#"{{"id":"x","result":{{"type":"session_snapshot","snapshot":{{"protocol":{protocol},"workspaces":[{{"workspace_id":"w1","label":"haste"}}],
              "agents":[{{"pane_id":"w1:p1","workspace_id":"w1","agent_status":"{status}","cwd":"/private"}}]}}}}}}"#
        );
        fs::write(self.path().join(format!("snapshot-{machine}.json")), json).unwrap();
    }

    fn touch(&self, name: &str) {
        fs::write(self.path().join(name), "").unwrap();
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.path().join("args.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn only_enabled_machines_are_discovered() {
    let fake = Fake::new();
    let machines = discover(&fake.runner(Duration::from_secs(5))).unwrap();
    assert_eq!(
        machines
            .iter()
            .map(|m| m.label.as_str())
            .collect::<Vec<_>>(),
        ["m1", "mbp"]
    );
    assert_eq!(fake.calls(), ["machine list --json"]);
}

#[test]
fn a_remote_snapshot_is_fetched_through_the_hubs_cli_and_parsed() {
    let fake = Fake::new();
    fake.snapshot("m1", 22, "blocked");
    let s = fetch_snapshot(&fake.runner(Duration::from_secs(5)), "m1").unwrap();
    assert_eq!(s.workspaces[0].label, "haste");
    assert_eq!(s.agents[0].agent_status, AgentStatus::Blocked);
    assert_eq!(fake.calls(), ["--machine m1 api snapshot"]);
}

#[test]
fn another_protocol_from_a_remote_is_a_mismatch_naming_both_numbers() {
    let fake = Fake::new();
    fake.snapshot("m1", 23, "idle");
    assert_eq!(
        fetch_snapshot(&fake.runner(Duration::from_secs(5)), "m1"),
        Err(FeedError::Mismatch {
            expected: 22,
            observed: 23
        })
    );
}

#[test]
fn a_failing_call_is_an_error_not_a_guess() {
    let fake = Fake::new();
    fake.touch("fail-m1");
    assert!(matches!(
        fetch_snapshot(&fake.runner(Duration::from_secs(5)), "m1"),
        Err(FeedError::Io(_))
    ));
}

#[test]
fn a_hanging_call_is_killed_at_the_timeout_together_with_its_children() {
    let fake = Fake::new();
    fake.touch("hang-m1"); // the script runs `sleep 31`
    let started = Instant::now();
    let result = fetch_snapshot(&fake.runner(Duration::from_millis(400)), "m1");
    assert_eq!(result, Err(FeedError::Io("herdr call timed out".into())));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "returned promptly: {:?}",
        started.elapsed()
    );
    // The kill is asynchronous from the test's point of view: give the system up to 3 seconds to
    // reap the killed process group before declaring a survivor.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let leftover = Command::new("pgrep")
            .args(["-f", "sleep 31"])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&leftover.stdout).trim().is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "the hung child survived");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_prompt_for_input_cannot_block_because_stdin_is_closed() {
    // `cat` with no file argument would wait on stdin forever; with stdin closed it ends at once.
    let dir = tempfile::tempdir().unwrap();
    let bin: PathBuf = dir.path().join("herdr");
    fs::write(&bin, "#!/bin/sh\nread answer\necho \"got:$answer\"\n").unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let mut runner = Runner::new(&bin);
    runner.timeout = Duration::from_secs(5);
    let started = Instant::now();
    let out = runner.run(&HerdrCommand::MachineList);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "did not wait for input"
    );
    assert!(out.is_ok() || matches!(out, Err(FeedError::Io(_))));
}

#[test]
fn across_a_full_run_only_the_four_allowed_shapes_are_ever_executed() {
    let fake = Fake::new();
    fake.snapshot("m1", 22, "idle");
    fake.snapshot("mbp", 22, "working");
    let runner = fake.runner(Duration::from_secs(5));
    for m in discover(&runner).unwrap() {
        fetch_snapshot(&runner, &m.label).unwrap();
    }
    runner
        .run(&HerdrCommand::Prompt {
            agent: "agent-a".into(),
            text: "result".into(),
        })
        .unwrap();
    runner
        .run(&HerdrCommand::RemotePrompt {
            label: "m1".into(),
            agent: "agent-a".into(),
            text: "result".into(),
        })
        .unwrap();

    let allowed = |c: &str| {
        c == "machine list --json"
            || (c.starts_with("--machine ")
                && c.ends_with(" api snapshot")
                && c.split_whitespace().count() == 4)
            || (c.starts_with("agent prompt ") && c.split_whitespace().count() == 4)
            || (c.starts_with("--machine ")
                && c.contains(" agent prompt ")
                && c.split_whitespace().count() == 6)
    };
    let calls = fake.calls();
    assert_eq!(calls.len(), 5, "{calls:?}");
    for c in &calls {
        assert!(allowed(c), "unexpected herdr call: {c}");
    }
    for forbidden in [
        "add",
        "remove",
        "enable",
        "disable",
        "rename",
        "reconnect",
        "send-keys",
        "pane",
        "read",
    ] {
        assert!(
            !calls
                .iter()
                .any(|c| c.split_whitespace().any(|w| w == forbidden)),
            "{forbidden} was run"
        );
    }
}

#[test]
fn a_disabled_machine_is_not_polled_by_a_loop_over_the_discovered_list() {
    let fake = Fake::new();
    fake.snapshot("m1", 22, "idle");
    fake.snapshot("mbp", 22, "idle");
    let runner = fake.runner(Duration::from_secs(5));
    for m in discover(&runner).unwrap() {
        fetch_snapshot(&runner, &m.label).unwrap();
    }
    assert!(
        !fake.calls().iter().any(|c| c.contains("m2")),
        "m2 is disabled and must not be contacted"
    );
}
