use cargo_metadata::MetadataCommand;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let metadata = MetadataCommand::new().exec()?;
    let workspace_root = metadata.workspace_root;
    prost_build::compile_protos(&["dookie.proto"], &[workspace_root])?;

    Ok(())
}
