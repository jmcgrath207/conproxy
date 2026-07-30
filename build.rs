fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                "proto/conproxy/v1/search.proto",
                "proto/conproxy/cdc/v1/cdc.proto",
            ],
            &["proto"],
        )?;
    Ok(())
}
