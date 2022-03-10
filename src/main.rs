use clap::Parser;
use serde_json;

mod multipathtunnel;
mod settings;
mod tasks;
mod messages;


#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(long)]
    config: String,
}


#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = Args::parse();

    let conf_path = args.config;

    let settings: settings::SettingsFile = serde_json::from_str(
        std::fs::read_to_string(conf_path)
            .unwrap()
            .as_str()
    ).unwrap();

    println!("Using settings: {:?}", settings);

    multipathtunnel::run(settings).await;
}
