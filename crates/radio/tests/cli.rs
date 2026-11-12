use std::process::Command;

#[test]
fn invalid_arguments_and_names_print_only_usage() {
    for args in [
        vec![],
        vec!["serve", "--name"],
        vec!["serve", "--other", "garden"],
        vec!["serve", "--name", "bad/name"],
        vec!["serve", "--name", ""],
        vec!["serve", "--name", "garden", "extra"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_radio"))
            .args(args)
            .env_remove("HOME")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, b"usage: radio serve [--name <name>]\n       radio install --app <name> --app-version <version> --handler <path> [--rooms <path>]\n       radio uninstall --app <name>\n       radio status\n");
    }
}

#[test]
fn missing_or_empty_home_has_a_fixed_error() {
    for args in [
        vec!["serve", "--name", "test-host"],
        vec![
            "install",
            "--app",
            "gardenapp",
            "--app-version",
            "2.1.0",
            "--handler",
            "/unused",
        ],
        vec!["uninstall", "--app", "gardenapp"],
        vec!["status"],
    ] {
        for empty in [false, true] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_radio"));
            command.args(&args);
            if empty {
                command.env("HOME", "");
            } else {
                command.env_remove("HOME");
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(1));
            assert!(output.stdout.is_empty());
            assert_eq!(output.stderr, b"radio: HOME is not set\n");
        }
    }
}

#[test]
fn install_accepts_each_flag_order_before_checking_home() {
    let flags = [
        ["--app", "gardenapp"],
        ["--app-version", "2.1.0"],
        ["--handler", "/unused"],
        ["--rooms", "/old/rooms.db"],
    ];
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if (0..4).any(|i| order[i + 1..].contains(&order[i])) {
                        continue;
                    }
                    let mut command = Command::new(env!("CARGO_BIN_EXE_radio"));
                    command.arg("install").env_remove("HOME");
                    for index in order {
                        command.args(flags[index]);
                    }
                    let output = command.output().unwrap();
                    assert_eq!(output.status.code(), Some(1));
                    assert!(output.stdout.is_empty());
                    assert_eq!(output.stderr, b"radio: HOME is not set\n");
                }
            }
        }
    }
}
