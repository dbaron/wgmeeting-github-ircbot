#[macro_use]
extern crate log;
extern crate env_logger;
extern crate irc;
extern crate github;

use irc::client::prelude::*;

fn main() {
    env_logger::init().unwrap();

    // This could be in a JSON config, but then we need to figure out how
    // to find that JSON config
    let irc_config : Config = Config {
        owners: Some(vec![format!("dbaron")]),
        nickname: Some(format!("wgmeeting-github-bot")),
        alt_nicks: Some(vec![format!("wgmeeting-github-bot-"), format!("wgmeeting-github-bot--")]),
        username: Some(format!("dbaron-gh-bot")),
        realname: Some(format!("Bot to add meeting minutes to github issues.")),
        server: Some(format!("irc.w3.org")),
        port: Some(6667),
        use_ssl: Some(false),
        encoding: Some(format!("UTF-8")),
        channels: Some(vec![format!("#cssbottest")]),
        user_info: Some(format!("Bot to add meeting minutes to github issues.")),
        //source: Some(format!("https://github.com/dbaron/wgmeeting-github-ircbot")), // FIXME: why doesn't this work as documented?
        .. Default::default()
    };

    let server = IrcServer::from_config(irc_config).unwrap();
    server.identify().unwrap();
    for message in server.iter() {
        let message = message.unwrap(); // panic if there's an error

        match message.command {
            Command::PRIVMSG(ref target, ref msg) => {
                match message.source_nickname() {
                    None => {
                        warn!("PRIVMSG without a source! {}", message); // FIXME: trailing \n
                    },
                    Some(ref source) => {
                        let mynick = server.current_nickname();
                        if target == mynick {
                            handle_bot_command(&server, msg, source, None)
                        } else if target.starts_with('#') {
                            match check_command_in_channel(mynick, msg) {
                                Some(ref command) => {
                                    handle_bot_command(&server, command, target, Some(source))
                                },
                                None => {
                                    // unimplemented!();
                                }
                            }
                        } else {
                            warn!("UNEXPECTED TARGET {} in message {}", target, message); // FIXME: trailing \n
                        }
                    }
                }
            },
            _ => (),
        }
    }
}

// Take a message in the channel, and see if it was a message sent to
// this bot.
fn check_command_in_channel(mynick: &str, msg: &String) -> Option<String> {
    if !msg.starts_with(mynick)  {
        return None;
    }
    let after_nick = &msg[mynick.len()..];
    if !after_nick.starts_with(":") && !after_nick.starts_with(",") {
        return None;
    }
    let after_punct = &after_nick[1..];
    Some(String::from(after_punct.trim_left()))
}

fn handle_bot_command(server: &IrcServer, command: &str, response_target: &str, response_username: Option<&str>) {

    let send_line = |response_username: Option<&str>, line: &str| {
        let send_line = match response_username {
            None => String::from(line),
            Some(username) => String::from(username) + ", " + line
        };
        server.send_privmsg(response_target, &send_line).unwrap();
    };

    if command == "help" {
        send_line(response_username, "The commands I understand are:");
        send_line(None, "  help     Send this message.");
        return;
    }

    send_line(response_username, "Sorry, I don't understand that command.  Try 'help'.");
}
