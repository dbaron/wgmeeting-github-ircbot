// see 'rustc -W help'
#![warn(missing_docs, unused_extern_crates, unused_results)]

//! An IRC bot that posts comments to github when W3C-style IRC minuting is
//! combined with "Github topic:" or "Github issue:" lines that give the
//! github issue to comment in.

extern crate env_logger;
extern crate irc;
extern crate tokio_core;
extern crate wgmeeting_github_ircbot;

use irc::client::prelude::{Client, ClientExt, Config, Future, IrcClient, Stream};
use irc::client::PackedIrcClient;
use std::env;
use std::fs;
use tokio_core::reactor::Core;
use wgmeeting_github_ircbot::*;

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

    let mut config = Config::load(config_file)
        .expect("couldn't load configuration file");
    let token = fs::read_to_string(token_file)
        .expect("couldn't read github access token file");
    let _ = config
        .options
        .as_mut()
        .unwrap()
        .insert("github_access_token".to_string(), token);
    config
}

fn main() {
    env_logger::init();
    let config: &'static Config = Box::leak(Box::new(read_config()));

    // FIXME: Add a way to ask the bot to reboot itself?

    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let mut irc_state = IRCState::new(GithubType::RealGithubConnection, &handle);

    let irc_client_future = IrcClient::new_future(handle, config).expect(
        "Couldn't initialize server \
         with given configuration file",
    );

    let PackedIrcClient(irc, irc_outgoing_future) = core.run(irc_client_future).unwrap();

    irc.identify().unwrap();

    let ircstream = irc.stream().for_each(|message| {
        let options = config.options.as_ref().unwrap();
        process_irc_message(&irc, &mut irc_state, options, message);
        Ok(())
    });

    let _ = core.run(ircstream.join(irc_outgoing_future)).unwrap();
}
