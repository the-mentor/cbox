//! `cbox down` — stop and remove a box.
//!
//! With detach:true the box outlives `cbox up`, so this is the only thing that
//! actually removes it.

use anyhow::{Context, Result};
use boxlite::{BoxliteError, BoxliteOptions, BoxliteRuntime};

use crate::{config, naming};

pub async fn run(name: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    let resolved = naming::resolve(name.as_deref(), &cwd);
    let home = config::box_home(&resolved.name);

    if !home.exists() {
        println!("cbox: no box home for {} — nothing to do", resolved.name);
        return Ok(());
    }

    let runtime = BoxliteRuntime::new(BoxliteOptions {
        home_dir: home.clone(),
        image_registries: vec![],
    })
    .context("failed to open the BoxLite runtime")?;

    // Removal is the goal; a box that is already gone is not an error — it's
    // the state `down` was trying to reach anyway.
    match runtime.remove(&resolved.name, true).await {
        Ok(()) => println!("cbox: removed {}", resolved.name),
        Err(BoxliteError::NotFound(_)) => {
            println!("cbox: no box named {} — nothing to do", resolved.name)
        }
        Err(e) => return Err(e).with_context(|| format!("failed to remove {}", resolved.name)),
    }

    // The home is per box name, so once the box is gone everything left in it
    // (image cache, disk-images, db) is that box's alone — multiple GB that
    // BoxLite's `remove` leaves behind. Only delete it if nothing else was
    // created in it by hand, and release the runtime's lock first.
    if !runtime.list_info().await.unwrap_or_default().is_empty() {
        return Ok(());
    }
    drop(runtime);
    std::fs::remove_dir_all(&home)
        .with_context(|| format!("failed to delete box home {}", home.display()))?;
    println!("cbox: deleted {}", home.display());
    Ok(())
}
