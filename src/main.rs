mod api;
mod audio;
mod map;
mod spectrum;
mod store;
mod ui;
mod visual;

use std::sync::mpsc;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use api::Api;
use audio::{Event, Player};

/// oud. your signal, in the terminal.
#[derive(Parser)]
#[command(name = "oud", version, about, disable_help_subcommand = true)]
struct Cli {
    /// API to talk to.
    #[arg(long, env = "OUD_API_URL", default_value = api::DEFAULT_BASE_URL, global = true)]
    api: String,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Play the best match for a search, without the interface.
    Play {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
    },
    /// List stations matching a search.
    Search {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let api = Api::new(&cli.api)?;
    match cli.command {
        None => ui::run(api),
        Some(Command::Search { query }) => search(&api, &query.join(" ")),
        Some(Command::Play { query }) => play(api, &query.join(" ")),
    }
}

fn search(api: &Api, query: &str) -> Result<()> {
    let stations = api.search(query)?;
    if stations.is_empty() {
        println!("nothing on that frequency.");
    }
    for s in stations {
        println!("▷ {:<40} {}", s.name.to_lowercase(), s.place().to_lowercase());
    }
    Ok(())
}

fn play(api: Api, query: &str) -> Result<()> {
    let Some(station) = api.search(query)?.into_iter().next() else {
        bail!("nothing on that frequency.");
    };
    let saved = store::Store::load();
    let (tx, rx) = mpsc::channel();
    let player = Player::new(api, saved.volume, tx)?;
    let tune = player.play(&station);

    println!("▷ {}", station.name.to_lowercase());
    for event in rx {
        match event {
            Event::Connecting { .. } => println!("  connecting…"),
            Event::OnAir { tune: t, format } if t == tune => println!("  on air · {format}"),
            Event::Title { tune: t, title } if t == tune => println!("  ♪ {}", title.to_lowercase()),
            Event::Reconnecting { .. } => println!("  signal lost, reconnecting…"),
            Event::Failed { reason, .. } => bail!("{reason}"),
            _ => {}
        }
    }
    Ok(())
}
