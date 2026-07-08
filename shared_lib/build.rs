

fn main() -> Result<(), Box<dyn std::error::Error>> {
    
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")?;
    let root_dir = std::path::Path::new(&manifest_dir).parent().unwrap();
    
    let proto_file = root_dir.join("proto/market_data.proto");
    let proto_dir = root_dir.join("proto");

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto_file], &[proto_dir])?;

    Ok(())
}