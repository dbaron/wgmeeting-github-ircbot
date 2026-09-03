// see 'rustc -W help'
#![warn(
    missing_docs,
    unused,
    unused_results,
    nonstandard_style,
    rust_2018_compatibility,
    rust_2018_idioms
)]

//! An IRC bot that posts comments to github when W3C-style IRC minuting is
//! combined with "Github topic:" or "Github issue:" lines that give the
//! github issue to comment in.

use anyhow::Result;
use futures::prelude::*;
use irc::client::prelude::{Client as IrcClient, Config as IrcConfig};
use serde::Deserialize;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::str;
use wgmeeting_github_ircbot::*;

fn read_config() -> (IrcConfig, BotConfig) {
    let mut args = env::args_os();
    if args.len() != 3 {
        eprintln!(
            "syntax: {} <config file> <github access token file>\n",
            env::args().next().unwrap()
        );
        ::std::process::exit(1);
    }
    let (_, config_file, token_file) = (
        args.next().unwrap(),
        args.next().unwrap(),
        args.next().unwrap(),
    );

    #[derive(Deserialize)]
    struct Config {
        irc: IrcConfig,
        bot: BotConfig,
        channels: HashMap<String, ChannelConfig>,
    }
    let file = fs::read(config_file).expect("couldn't load configuration file");
    let file_contents = str::from_utf8(&file).expect("configuration file not UTF-8");
    let mut config: Config =
        toml::from_str(file_contents).expect("couldn't parse configuration file");
    config.bot.github_access_token =
        fs::read_to_string(token_file).expect("couldn't read github access token file");
    config.irc.channels = config.channels.keys().cloned().collect();
    config.bot.channels = config.channels;
    (config.irc, config.bot)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::init();
    let (irc_config, bot_config) = read_config();
    let bot_config: &'static _ = Box::leak(Box::new(bot_config));

    // FIXME: Add a way to ask the bot to reboot itself?

    let mut irc_state = IRCState::new(GithubType::RealGithubConnection);

    let irc_client: &'static mut _ = Box::leak(Box::new(IrcClient::from_config(irc_config).await?));
    irc_client.identify()?;

    let mut irc_stream = irc_client.stream()?;

    while let Some(message) = irc_stream.next().await.transpose()? {
        process_irc_message(irc_client, &mut irc_state, bot_config, message);
    }

    Ok(())
}
