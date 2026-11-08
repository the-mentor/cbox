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
    for args in [&["profile"][..], &["profile", "create"], &["profile", "edit"], &["profile", "delete"]] {
        let out = run(args);
        let text = String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && text.contains("Usage:"), "{args:?}: {out:?}");
    }
    assert_eq!(run(&["profile", "bogus"]).status.code(), Some(2), "real usage errors keep exit 2");
    std::fs::remove_dir_all(&d).ok();
}
