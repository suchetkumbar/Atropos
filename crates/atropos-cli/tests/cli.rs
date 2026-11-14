use std::process::Command;

#[test]
fn version_prints_name_and_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_atr"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("atr "));
}

#[test]
fn missing_gui_exits_1_with_message() {
    let dir = std::env::temp_dir().join(format!("atropos cli test ü {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let atr = dir.join("atr.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_atr"), &atr).unwrap();
    let out = Command::new(&atr).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Atropos.exe not found next to"));
}

#[test]
fn ctrl_c_helper_rejects_bad_arguments_with_exit_2() {
    for args in [
        vec!["__ctrl-c"],
        vec!["__ctrl-c", "x", "1"],
        vec!["__ctrl-c", "1"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_atr"))
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn ctrl_c_helper_refuses_a_key_that_does_not_exist() {
    // PID 4 is System; create time 1 can never match it, so identity validation fails (exit 1)
    // and nothing is sent.
    let out = Command::new(env!("CARGO_BIN_EXE_atr"))
        .args(["__ctrl-c", "4", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[allow(clippy::unwrap_used)] // test code
mod commands {
    use std::net::TcpListener;
    use std::os::windows::process::CommandExt as _;
    use std::process::{Command, Output, Stdio};
    use std::time::{Duration, Instant};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn atr(args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_atr"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn text(o: &Output) -> String {
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    #[test]
    fn port_finds_this_test_as_the_owner() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port().to_string();
        let o = atr(&["port", &port, "--json"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o));
        assert!(text(&o).starts_with("{\"schema\":1,"));
        assert!(text(&o).contains(&format!("\"pid\":{}", std::process::id())));
    }

    #[test]
    fn unused_port_exits_1() {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert_eq!(atr(&["port", &port.to_string()]).status.code(), Some(1));
    }

    #[test]
    fn dry_run_refuses_to_end_its_own_parent() {
        // The owner is this test, which is atr's parent: protected, and a dry run anyway.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port().to_string();
        let o = atr(&["port", &port, "--kill", "--dry-run", "--json"]);
        assert_eq!(o.status.code(), Some(2), "{}", text(&o));
        assert!(text(&o).contains("\"outcome\":\"refused\""));
        assert!(l.local_addr().is_ok(), "still alive and listening");
    }

    #[test]
    fn wait_port_times_out_then_sees_it_free() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port().to_string();
        assert_eq!(
            atr(&["wait-port", &port, "--timeout", "400ms"])
                .status
                .code(),
            Some(4)
        );
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            drop(l);
        });
        let start = Instant::now();
        assert_eq!(
            atr(&["wait-port", &port, "--timeout", "10s"]).status.code(),
            Some(0)
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        t.join().unwrap();
    }

    #[test]
    fn find_and_tree_show_this_test() {
        let me = std::process::id().to_string();
        let o = atr(&["find", &format!("pid:{me}"), "--json"]);
        assert_eq!(o.status.code(), Some(0));
        assert!(text(&o).contains(&format!("\"pid\":{me}")));
        let o = atr(&["tree", &me]);
        assert_eq!(o.status.code(), Some(0));
        assert!(
            text(&o).contains("atr.exe"),
            "atr runs as our child: {}",
            text(&o)
        );
    }

    #[test]
    fn kill_without_a_terminal_needs_yes_then_force_ends_the_victim() {
        // Victim: our own child, on its own hidden console (Ctrl+C can never reach the runner).
        let held = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = held.local_addr().unwrap().port().to_string();
        let mut victim = Command::new(env!("CARGO_BIN_EXE_atr"))
            .args(["wait-port", &port, "--timeout", "60s"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = victim.id().to_string();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(atr(&["kill", &pid]).status.code(), Some(1), "not confirmed");
        assert!(victim.try_wait().unwrap().is_none(), "still running");
        let o = atr(&["kill", &pid, "--yes", "--force", "--json"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o));
        assert!(text(&o).contains("\"outcome\":\"terminated\""));
        let status = victim.wait().unwrap();
        assert!(!status.success());
        drop(held);
    }
}
