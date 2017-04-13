#[macro_use]
extern crate log;
extern crate env_logger;
extern crate irc;
extern crate github;
#[macro_use]
extern crate lazy_static;
extern crate regex;

use irc::client::prelude::*;
use regex::Regex;

fn main() {
    env_logger::init().unwrap();

    // This could be in a JSON config, but then we need to figure out how
    // to find that JSON config
    let irc_config: Config = Config {
        owners: Some(vec![format!("dbaron")]),
        nickname: Some(format!("wgmeeting-github-bot")),
        alt_nicks: Some(vec![format!("wgmeeting-github-bot-"),
                             format!("wgmeeting-github-bot--")]),
        username: Some(format!("dbaron-gh-bot")),
        realname: Some(format!("Bot to add meeting minutes to github issues.")),
        server: Some(format!("irc.w3.org")),
        port: Some(6667),
        use_ssl: Some(false),
        encoding: Some(format!("UTF-8")),
        channels: Some(vec![format!("#cssbottest")]),
        user_info: Some(format!("Bot to add meeting minutes to github issues.")),
        // FIXME: why doesn't this work as documented?
        //source: Some(format!("https://github.com/dbaron/wgmeeting-github-ircbot")),
        ..Default::default()
    };

    // FIXME: Eventually this should support multiple channels, plus
    // options to ask the bot which channels it's in, and which channels
    // it currently has buffers in.  (Then we can do things like ask the
    // bot to reboot itself, but it will only do so if it's not busy.)
    let mut channel_data = ChannelData::new();

    let server = IrcServer::from_config(irc_config).unwrap();
    server.identify().unwrap();
    for message in server.iter() {
        let message = message.unwrap(); // panic if there's an error

        match message.command {
            Command::PRIVMSG(ref target, ref msg) => {
                match message.source_nickname() {
                    None => {
                        // FIXME: trailing \n
                        warn!("PRIVMSG without a source! {}", message);
                    }
                    Some(ref source) => {
                        let mynick = server.current_nickname();
                        if target == mynick {
                            handle_bot_command(&server, msg, source, None)
                        } else if target.starts_with('#') {
                            let source_ = String::from(*source);
                            let line = if msg.starts_with("\x01ACTION ") && msg.ends_with("\x01") {
                                ChannelLine {
                                    source: source_,
                                    is_action: true,
                                    message: String::from(&msg[8..msg.len() - 1]),
                                }
                            } else {
                                ChannelLine {
                                    source: source_,
                                    is_action: false,
                                    message: msg.clone(),
                                }
                            };

                            // FIXME: This needs to handle requests in /me
                            match check_command_in_channel(mynick, msg) {
                                Some(ref command) => {
                                    handle_bot_command(&server, command, target, Some(source))
                                }
                                None => {
                                    match channel_data.add_line(line) {
                                        None => (),
                                        Some(response) => {
                                            server.send_privmsg(target, &*response).unwrap();
                                        }
                                    }
                                }
                            }
                        } else {
                            // FIXME: trailing \n
                            warn!("UNEXPECTED TARGET {} in message {}", target, message);
                        }
                    }
                }
            }
            _ => (),
        }
    }
}

// Take a message in the channel, and see if it was a message sent to
// this bot.
fn check_command_in_channel(mynick: &str, msg: &String) -> Option<String> {
    if !msg.starts_with(mynick) {
        return None;
    }
    let after_nick = &msg[mynick.len()..];
    if !after_nick.starts_with(":") && !after_nick.starts_with(",") {
        return None;
    }
    let after_punct = &after_nick[1..];
    Some(String::from(after_punct.trim_left()))
}

fn handle_bot_command(server: &IrcServer,
                      command: &str,
                      response_target: &str,
                      response_username: Option<&str>) {

    let send_line = |response_username: Option<&str>, line: &str| {
        let adjusted_line = match response_username {
            None => String::from(line),
            Some(username) => String::from(username) + ", " + line,
        };
        server
            .send_privmsg(response_target, &adjusted_line)
            .unwrap();
    };

    if command == "help" {
        send_line(response_username, "The commands I understand are:");
        send_line(None, "  help     Send this message.");
        return;
    }

    send_line(response_username,
              "Sorry, I don't understand that command.  Try 'help'.");
}

struct ChannelLine {
    source: String,
    is_action: bool,
    message: String,
}

struct TopicData {
    topic: String,
    github_url: Option<String>,
    lines: Vec<ChannelLine>,
}

struct ChannelData {
    current_topic: Option<TopicData>,
}

impl TopicData {
    fn new(topic: &str) -> TopicData {
        let topic_ = String::from(topic);
        TopicData {
            topic: topic_,
            github_url: None,
            lines: vec![],
        }
    }
}

fn ci_starts_with(s: &str, prefix: &str) -> bool {
    assert!(prefix.to_lowercase() == prefix);

    s.len() >= prefix.len() && s[0..prefix.len()].to_lowercase() == prefix
}

fn strip_ci_prefix(s: &str, prefix: &str) -> Option<String> {
    if ci_starts_with(s, prefix) {
        Some(String::from(s[prefix.len()..].trim_left()))
    } else {
        None
    }
}

impl ChannelData {
    fn new() -> ChannelData {
        ChannelData { current_topic: None }
    }

    // Returns the response that should be sent to the message over IRC.
    fn add_line(&mut self, line: ChannelLine) -> Option<String> {
        match strip_ci_prefix(&line.message, "topic:") {
            None => (),
            Some(ref topic) => {
                self.start_topic(line.message[6..].trim_left());
            }
        }
        if line.source == "trackbot" && line.is_action == true &&
           line.message == "is ending a teleconference." {
            self.end_topic();
        }
        match self.current_topic {
            None => None,
            Some(ref mut data) => {
                let new_url_option = extract_github_url(&line.message);
                let response = match (new_url_option.as_ref(), data.github_url.as_ref()) {
                    (None, _) => None,
                    // FIXME: Add and implement instructions to cancel!
                    (Some(new_url), None) => {
                        Some(format!("OK, I'll post this discussion to {}", new_url))
                    }
                    (Some(new_url), Some(old_url)) if old_url == new_url => None,
                    (Some(new_url), Some(old_url)) => {
                        Some(format!("OK, I'll post this discussion to {} instead of {} like you said before",
                                     new_url,
                                     old_url))
                    }
                };

                if let Some(new_url) = new_url_option {
                    data.github_url = Some(new_url);
                }

                data.lines.push(line);

                response
            }
        }
    }

    fn start_topic(&mut self, topic: &str) {
        if self.current_topic.is_some() {
            self.end_topic();
        }

        self.current_topic = Some(TopicData::new(topic));
    }

    fn end_topic(&mut self) {
        // TODO: Test the topic boundary code.
        // FIXME: Do something with the data rather than throwing it away!
        self.current_topic = None;
    }
}

fn extract_github_url(message: &str) -> Option<String> {
    lazy_static! {
        static ref GITHUB_URL_RE: Regex =
            Regex::new(r"^https://github.com/(?P<repo>[^/]*/[^/]*)/issues/(?P<number>[0-9]+)$")
            .unwrap();
        static ref ALLOWED_REPOS: [String; 1] = [format!("dbaron/wgmeeting-github-ircbot")];
    }
    for prefix in ["topic:", "github topic:"].into_iter() {
        match strip_ci_prefix(&message, prefix) {
            None => (),
            Some(ref maybe_url) => {
                match GITHUB_URL_RE.captures(maybe_url) {
                    None => (),
                    Some(ref caps) => {
                        for repo in ALLOWED_REPOS.into_iter() {
                            if caps["repo"] == *repo {
                                return Some(maybe_url.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}
