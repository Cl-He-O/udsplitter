use udsplitter_server::*;
use utils::config_from_arg;

#[tokio::main]
async fn main() {
    start(config_from_arg()).await;
}
