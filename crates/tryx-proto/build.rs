use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = PathBuf::from("proto/wire-v1");
    let protos: Vec<PathBuf> = ["transport.proto", "configuration.proto", "overlay.proto"]
        .iter()
        .map(|name| proto_dir.join(name))
        .collect();
    for proto in &protos {
        println!("cargo:rerun-if-changed={}", proto.display());
    }
    prost_build::compile_protos(&protos, &[proto_dir])?;
    Ok(())
}
