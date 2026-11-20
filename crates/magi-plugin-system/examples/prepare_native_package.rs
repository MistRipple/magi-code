//! Verify and prepare one original npm artifact, without installing dependencies or activating it.
use magi_plugin_system::npm::NativePackage;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 {
        return Err(
            "usage: prepare_native_package <archive> <integrity> <name> <version> <empty-stage>"
                .into(),
        );
    }
    let package = NativePackage::prepare(
        &std::fs::read(&args[0])?,
        &args[1],
        &args[2],
        &args[3],
        std::path::Path::new(&args[4]),
    )?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "identity":package.identity(),"directory":package.directory(),"manifestSha256":package.manifest_sha256(),"status":"prepared-original","activation":"not-run"
        }))?
    );
    Ok(())
}
