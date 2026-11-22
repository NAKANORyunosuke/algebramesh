//! Generates the gRPC code from `proto/algebramesh.proto` with a vendored protoc,
//! so no system protoc is needed.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../proto/algebramesh.proto");
    let mut config = prost_build::Config::new();
    config.protoc_executable(protoc_bin_vendored::protoc_bin_path()?);
    tonic_prost_build::configure().compile_with_config(
        config,
        &["../../proto/algebramesh.proto"],
        &["../../proto"],
    )?;
    Ok(())
}
