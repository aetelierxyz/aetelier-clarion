use std::{fs, path::Path};

use clarion::idl::{DEVNET_INSTANCES, idl_path, render};

fn main() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for program_id in DEVNET_INSTANCES {
        let idl = render(crate_dir, program_id)
            .expect("clarion source renders as a Codama IDL");
        fs::write(idl_path(crate_dir, program_id), idl)
            .expect("idl directory is writable");
    }
}
