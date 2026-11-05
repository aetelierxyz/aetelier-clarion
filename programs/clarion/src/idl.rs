use std::path::{Path, PathBuf};

use codama::{Codama, CodamaResult, NodeTrait};

pub const DEVNET_INSTANCES: [&str; 1] = ["BjLoUKJ48wxVZXPEuR5JSmuXSkC5YqBvtN5i2cT51y3e"];

pub fn render(crate_dir: &Path, program_id: &str) -> CodamaResult<String> {
    let mut root = Codama::load(crate_dir)?.get_idl()?;
    root.program.public_key = program_id.to_string();
    Ok(root.to_json_pretty()? + "\n")
}

pub fn idl_path(crate_dir: &Path, program_id: &str) -> PathBuf {
    crate_dir.join("idl").join(format!("{program_id}.json"))
}
