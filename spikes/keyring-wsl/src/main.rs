//! THROWAWAY SPIKE (branch spike/keyring-wsl): does the stock `keyring` crate
//! (default `v1` feature, no custom store) work from a Linux binary inside WSL?
//! Run `cargo run` in WSL and paste the output back.
use keyring::Entry;
use std::time::Instant;

fn main() {
    let wsl = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.to_lowercase().contains("microsoft"))
        .unwrap_or(false);
    println!("wsl detected: {wsl}");
    println!("DBUS_SESSION_BUS_ADDRESS set: {}", std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some());

    let t = Instant::now();
    let status = Entry::store_status();
    println!("store_status: {status:?} ({:?})", t.elapsed());

    let secret = "pässwörd-€-םולש";
    let steps = || -> keyring::Result<()> {
        let e = Entry::new("cbox-spike", "roundtrip")?;
        let t = Instant::now();
        e.set_password(secret)?;
        println!("set ok ({:?})", t.elapsed());
        let t = Instant::now();
        let got = e.get_password()?;
        println!("get ok ({:?}), matches: {}", t.elapsed(), got == secret);
        e.delete_credential()?;
        println!("delete ok");
        println!("get after delete: {:?}", e.get_password());
        Ok(())
    };
    if let Err(err) = steps() {
        println!("FAILED: {err:?}");
        std::process::exit(1);
    }
}
