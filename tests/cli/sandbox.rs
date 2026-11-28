//! A confined step and where it may write.

use crate::support::{Project, CONFINED};

#[test]
fn a_confined_step_writes_only_where_it_may() {
    // A port the steps can reach unless their network is taken away.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let confined = CONFINED.replace("PORT", &port);
    let project = Project::new(&confined);
    project.write("src/.keep", "");
    let run = project.flot(&["run"]);
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        assert_eq!(run.code, 1, "{}", run.stdout);
        run.has("ran      inside               ok")
            .has("ran      outside              failed")
            .has("link refused")
            .has(if cfg!(target_os = "linux") {
                "b.txt: Permission denied"
            } else {
                "b.txt: Operation not permitted"
            })
            .has("ran      online               ok");
        assert!(!project.path().join("b.txt").exists());
        assert!(!project.path().join("src/flot.toml").exists());
        let inside = project.flot(&["--json", "show"]).json()["steps"][0]["receipt_id"]
            .as_str()
            .unwrap()
            .to_string();
        let receipt = project.flot(&["receipt", &inside]).json();
        assert_eq!(receipt["identity"]["sandbox"]["write"][0], "src");
        assert!(receipt["identity"]["sandbox"]["mechanism"]
            .as_str()
            .unwrap()
            .contains(if cfg!(target_os = "linux") {
                "landlock abi"
            } else {
                "seatbelt file writes"
            }));
        // A host that does not let an unprivileged process have a network
        // namespace must not run the step at all; any other host must run it
        // offline.
        if cfg!(target_os = "macos")
            || !run
                .stdout
                .contains("offline: could not run: its sandbox cannot be enforced here")
        {
            run.has("ran      offline              ok");
        }
        // The scratch directory a confined step wrote in is gone.
        let leftovers: Vec<_> = std::fs::read_dir(project.path().join(".flot/tmp"))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("sandbox-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    } else {
        assert_eq!(run.code, 1, "{}", run.stdout);
        run.has(
            "could not run: its sandbox cannot be enforced here: flot confines steps on Linux and macOS only",
        );
        assert!(!project.path().join("src/a.txt").exists());
    }

    // A path to write beneath must exist, or the step does not run.
    project.write(
        "flot.toml",
        &confined.replace("write = [\"src\"]", "write = [\"gone\"]"),
    );
    project
        .flot(&["run", "--only", "outside"])
        .has("could not run: its sandbox cannot be enforced here");
    project.write(
        "flot.toml",
        &confined.replace("network = false", "network = true"),
    );
    project
        .flot(&["plan"])
        .has("has a sandbox that restricts nothing");
}
