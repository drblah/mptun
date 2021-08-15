use clap::{App, load_yaml};
use serde_json;

mod multipathtunnel;
mod settings;

#[tokio::main]
async fn main() {
    let yaml = load_yaml!("cli.yaml");
    let matches = App::from(yaml).get_matches();

    let conf_path = match matches.value_of("config") {
        Some(value) => value,
        _ => panic!("Failed to get config file path. Does it point to a valid path?")
    };

    let settings: settings::SettingsFile = serde_json::from_str(
        std::fs::read_to_string(conf_path)
            .unwrap()
            .as_str()
    ).unwrap();

    println!("Using settings: {:?}", settings);

    multipathtunnel::run(settings).await;
}
