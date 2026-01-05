use std::fs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rev = fs::read_to_string("../../abi/dex/REVISION")?;
    println!("cargo::rustc-env=DEX_REVISION={rev}");
    Ok(())
}
