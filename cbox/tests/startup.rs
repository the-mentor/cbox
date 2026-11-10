//! Runs the built binary: `cbox --help` must still create profiles.toml (clap exits on --help).

#[test]
fn help_creates_profiles_toml_with_an_empty_default() {
    let d = std::env::temp_dir().join(format!("cbox-startup-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cbox"))
        .arg("--help")
        .env("XDG_CONFIG_HOME", &d)
        .env("HOME", &d)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let body = std::fs::read_to_string(d.join("cbox/profiles.toml")).unwrap();
    std::fs::remove_dir_all(&d).ok();
    assert_eq!(body, "[default]\n");
}

#[test]
fn a_command_without_its_arguments_prints_help_and_exits_zero() {
    let d = std::env::temp_dir().join(format!("cbox-noargs-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_cbox"))
            .args(args)
            .env("XDG_CONFIG_HOME", &d)
            .env("HOME", &d)
            .output()
            .unwrap()
    };
    for args in [&["profile"][..], &["profile", "create"], &["profile", "edit"], &["profile", "delete"], &["secret"], &["secret", "create"], &["secret", "edit"], &["secret", "delete"]] {
        let out = run(args);
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && text.contains("Usage:"), "{args:?}: {out:?}");
    }
    assert_eq!(run(&["profile", "bogus"]).status.code(), Some(2), "real usage errors keep exit 2");
    assert_eq!(run(&["secret", "create", "n"]).status.code(), Some(2));
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn completion_without_a_shell_prints_help_and_a_known_shell_prints_a_script() {
    let d = std::env::temp_dir().join(format!("cbox-completion-{}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    std::fs::create_dir_all(&d).unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_cbox"))
            .args(args)
            .env("XDG_CONFIG_HOME", &d)
            .env("HOME", &d)
            .output()
            .unwrap()
    };
    let out = run(&["completion"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && text.contains("Usage:"), "{out:?}");
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        assert!(text.contains(shell), "{shell} missing: {text}");
    }
    // `completion` runs from shell startup files: it must not create profiles.toml as a side effect.
    let profiles = d.join("cbox/profiles.toml");
    assert!(!profiles.exists(), "`completion` without a shell must not create profiles.toml");
    let out = run(&["completion", "bash"]);
    assert!(out.status.success() && String::from_utf8_lossy(&out.stdout).contains("_cbox"), "{out:?}");
    assert!(!profiles.exists(), "`completion bash` must not create profiles.toml");
    assert_eq!(run(&["completion", "nosuchshell"]).status.code(), Some(2));
    std::fs::remove_dir_all(&d).ok();
}
