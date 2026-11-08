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
