//! Recompile when a migration changes: `include_dir!` reads the crate's own `migrations` folder.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
