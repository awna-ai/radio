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
        assert_eq!(output.stderr, b"usage: radio serve [--name <name>]\n");
    }
}

#[test]
fn missing_or_empty_home_has_a_fixed_error() {
    for empty in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_radio"));
        command.args(["serve", "--name", "test-host"]);
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
