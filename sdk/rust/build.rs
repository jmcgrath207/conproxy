fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = std::path::Path::new("../../proto/conproxy/v1/search.proto");
    let (proto, include) = if workspace.exists() {
        ("../../proto/conproxy/v1/search.proto", "../../proto")
    } else {
        ("proto/conproxy/v1/search.proto", "proto")
    };
    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .compile_protos(&[proto], &[include])?;
    Ok(())
}
