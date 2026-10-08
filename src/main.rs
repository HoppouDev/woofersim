use bevy::app::App;
use tracing::level_filters::LevelFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::DEBUG)
        .compact()
        .without_time()
        .init();

    App::new().run();

    Ok(())
}
