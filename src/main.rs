// see 'rustc -W help'
#![warn(missing_docs, unused_extern_crates, unused_results)]

//! An IRC bot that posts comments to github when W3C-style IRC minuting is
//! combined with "Github topic:" or "Github issue:" lines that give the
//! github issue to comment in.

extern crate env_logger;
extern crate irc;
extern crate wgmeeting_github_ircbot;

use irc::client::prelude::*;
use std::env;
use wgmeeting_github_ircbot::*;

fn main() {
    env_logger::init();

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

    let server = IrcServer::new(config_file).expect(
        "Couldn't initialize server \
         with given configuration file",
    );

    server.identify().unwrap();

    let options = server.config().options.as_ref().expect(
        "No options property \
         within configuration?",
    );

    // FIXME: Add a way to ask the bot to reboot itself?
    let mut irc_state = IRCState::new(GithubType::RealGithubConnection);
    for message in server.iter() {
        let message = message.unwrap(); // panic if there's an error
        main_loop_iteration(server.clone(), &mut irc_state, options, &message);
    }
}
