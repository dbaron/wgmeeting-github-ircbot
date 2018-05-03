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
use std::ffi::OsString;
use std::fs::File;
use std::io::Read;
use tokio_core::reactor::Core;
use wgmeeting_github_ircbot::*;

fn main() {
    env_logger::init();

    lazy_static! {
        static ref ARGS: Vec<OsString> = {
            let args = env::args_os().skip(1).collect::<Vec<_>>();
            if args.len() != 2 {
                eprintln!("syntax: {} <config file> <github access token file>\n",
                    env::args_os().nth(0)
                        .map(|osstring| osstring.into_string().unwrap_or(String::from("")))
                        .unwrap_or(String::from("")));
                ::std::process::exit(1);
            }
            args
        };
        static ref CONFIG: Config = {
            Config::load(ARGS[0].clone()).expect("couldn't load configuration file")
        };
        static ref GITHUB_ACCESS_TOKEN: String = {
            let mut f = File::open(ARGS[1].clone())
                .expect("github access token file (second argument) not found");
            let mut token = String::new();
            let _numbytes =
                f.read_to_string(&mut token).expect("couldn't read github access token file");
            token
        };
        static ref OPTIONS: HashMap<String, String> = {
            let mut options =
                CONFIG.options.as_ref().expect("No options property within configuration?").clone();
            // Store the access token with the options, even though we read it from a different
            // place.
            let _oldval =
                options.insert("github_access_token".to_string(), GITHUB_ACCESS_TOKEN.to_string());
            options
        };
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
