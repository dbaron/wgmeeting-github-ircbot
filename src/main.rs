// see 'rustc -W help'
#![warn(missing_docs, unused_extern_crates, unused_results)]

//! An IRC bot that posts comments to github when W3C-style IRC minuting is
//! combined with "Github topic:" or "Github issue:" lines that give the
//! github issue to comment in.

extern crate env_logger;
extern crate irc;
#[macro_use]
extern crate lazy_static;
extern crate tokio_core;
extern crate wgmeeting_github_ircbot;

use irc::client::prelude::{Client, ClientExt, Config, Future, IrcClient, Stream};
use irc::client::PackedIrcClient;
use std::collections::HashMap;
use std::env;
use tokio_core::reactor::Core;
use wgmeeting_github_ircbot::*;

fn main() {
    env_logger::init();

    lazy_static! {
        static ref CONFIG: Config = {
            let config_file = {
                let mut args = env::args_os().skip(1); // skip program name
                let config_file = args.next().expect(
                    "Expected a single command-line argument, the JSON \
                     configuration file.",
                );
                if args.next().is_some() {
                    panic!("Expected only a single command-line argument, the JSON configuration file.");
                }
                config_file
            };
            Config::load(config_file).expect("couldn't load configuration file")
        };
        static ref OPTIONS: HashMap<String, String> = CONFIG.options.as_ref().expect("No options property within configuration?").clone();
    }

    // FIXME: Add a way to ask the bot to reboot itself?

    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let mut irc_state = IRCState::new(GithubType::RealGithubConnection, &handle);

    let irc_client_future = IrcClient::new_future(handle, &CONFIG).expect(
        "Couldn't initialize server \
         with given configuration file",
    );

    let PackedIrcClient(irc, irc_outgoing_future) = core.run(irc_client_future).unwrap();

    irc.identify().unwrap();

    let ircstream = irc.stream().for_each(|message| {
        process_irc_message(&irc, &mut irc_state, &OPTIONS, message);
        Ok(())
    });

    let _ = core.run(ircstream.join(irc_outgoing_future)).unwrap();
}
