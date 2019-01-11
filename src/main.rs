// see 'rustc -W help'
#![warn(missing_docs, unused_extern_crates, unused_results)]

//! An IRC bot that posts comments to github when W3C-style IRC minuting is
//! combined with "Github topic:" or "Github issue:" lines that give the
//! github issue to comment in.

extern crate env_logger;
extern crate irc;
// We need this for derive(Deserialize).
#[allow(unused_extern_crates)]
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate tokio_core;
extern crate toml;
extern crate wgmeeting_github_ircbot;

use irc::client::prelude::{Client, ClientExt, Config as IrcConfig, Future, IrcClient, Stream};
use irc::client::PackedIrcClient;
use std::collections::HashMap;
use std::env;
use std::fs;
use tokio_core::reactor::Core;
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
    let mut config: Config =
        toml::from_slice(&file).expect("couldn't parse configuration file");
    config.bot.github_access_token =
        fs::read_to_string(token_file).expect("couldn't read github access token file");
    config.irc.channels = Some(config.channels.keys().cloned().collect());
    config.bot.channels = config.channels;
    (config.irc, config.bot)
}

fn main() {
    env_logger::init();
    let (irc_config, bot_config): &'static (_, _) = Box::leak(Box::new(read_config()));

    // FIXME: Add a way to ask the bot to reboot itself?

    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let mut irc_state = IRCState::new(GithubType::RealGithubConnection, &handle);

    let irc_client_future = IrcClient::new_future(handle, irc_config).expect(
        "Couldn't initialize server \
         with given configuration file",
    );

    let PackedIrcClient(irc, irc_outgoing_future) = core.run(irc_client_future).unwrap();

    irc.identify().unwrap();

    let ircstream = irc.stream().for_each(|message| {
        process_irc_message(&irc, &mut irc_state, bot_config, message);
        Ok(())
    });

    let _ = core.run(ircstream.join(irc_outgoing_future)).unwrap();
}
