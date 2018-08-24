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
extern crate serde_json;
extern crate tokio_core;
extern crate wgmeeting_github_ircbot;

use irc::client::prelude::{Client, ClientExt, Config as IrcConfig, Future, IrcClient, Stream};
use irc::client::PackedIrcClient;
use std::env;
use std::fs::{self, File};
use tokio_core::reactor::Core;
use wgmeeting_github_ircbot::*;

#[derive(Deserialize)]
struct Config {
    irc: IrcConfig,
    bot: BotConfig,
}

fn read_config() -> Config {
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

    let file = File::open(config_file).expect("couldn't open configuration file");
    let mut config: Config =
        serde_json::from_reader(file).expect("couldn't load configuration file");
    config.bot.github_access_token =
        fs::read_to_string(token_file).expect("couldn't read github access token file");
    config.irc.channels = Some(config.bot.channels.keys().cloned().collect());
    config
}

fn main() {
    env_logger::init();
    let config: &'static Config = Box::leak(Box::new(read_config()));

    // FIXME: Add a way to ask the bot to reboot itself?

    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let mut irc_state = IRCState::new(GithubType::RealGithubConnection, &handle);

    let irc_client_future = IrcClient::new_future(handle, &config.irc).expect(
        "Couldn't initialize server \
         with given configuration file",
    );

    let PackedIrcClient(irc, irc_outgoing_future) = core.run(irc_client_future).unwrap();

    irc.identify().unwrap();

    let ircstream = irc.stream().for_each(|message| {
        process_irc_message(&irc, &mut irc_state, &config.bot, message);
        Ok(())
    });

    let _ = core.run(ircstream.join(irc_outgoing_future)).unwrap();
}
