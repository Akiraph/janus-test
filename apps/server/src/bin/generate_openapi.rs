fn main() -> anyhow::Result<()> {
    println!(
        "{}",
        janus_server::transport::http::openapi().to_pretty_json()?
    );
    Ok(())
}
