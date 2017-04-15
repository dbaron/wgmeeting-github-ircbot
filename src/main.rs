#[macro_use]
extern crate log;
extern crate env_logger;
extern crate irc;
#[macro_use]
extern crate lazy_static;
extern crate regex;
extern crate hyper;
extern crate hubcaps;
extern crate hyper_native_tls;
extern crate serde_json;

use std::fmt;
use std::thread;
use std::collections::HashMap;
use regex::Regex;

use irc::client::prelude::*;
use irc::client::data::command::Command;

use hyper::Client;
use hyper::net::HttpsConnector;
use hyper_native_tls::NativeTlsClient;
use hubcaps::{Credentials, Github};
use hubcaps::comments::CommentOptions;

fn main() {
    env_logger::init().unwrap();

    let server =
        IrcServer::new("config.json").expect("Couldn't initialize server with config.json");
    server.identify().unwrap();

    let options = server
        .config()
        .options
        .as_ref()
        .expect("No options property within configuration?");

    // FIXME: Add options to ask the bot which channels it's in, and
    // which channels it currently has buffers in.  (Then we can do
    // things like ask the bot to reboot itself, but it will only do so
    // if it's not busy.)
    let mut channel_data: HashMap<String, ChannelData> = HashMap::new();

    for message in server.iter() {
        let message = message.unwrap(); // panic if there's an error

        match message.command {
            Command::PRIVMSG(ref target, ref msg) => {
                match message.source_nickname() {
                    None => {
                        warn!("PRIVMSG without a source! {}",
                              format!("{}", message).trim());
                    }
                    Some(ref source) => {
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
                        let mynick = server.current_nickname();
                        if target == mynick {
                            // An actual private message.
                            info!("[{}] {}", source, line);
                            handle_bot_command(&server,
                                               options,
                                               None,
                                               &line.message,
                                               source,
                                               false,
                                               None)
                        } else if target.starts_with('#') {
                            // A message in a channel.
                            info!("[{}] {}", target, line);
                            let this_channel_data =
                                channel_data
                                    .entry(target.clone())
                                    .or_insert_with(|| ChannelData::new(&options));
                            match check_command_in_channel(mynick, &line.message) {
                                Some(ref command) => {
                                    handle_bot_command(&server,
                                                       options,
                                                       Some(this_channel_data),
                                                       command,
                                                       target,
                                                       line.is_action,
                                                       Some(source))
                                }
                                None => {
                                    if let Some(response) = this_channel_data.add_line(line) {
                                        server.send_privmsg(target, &*response).unwrap();
                                    }
                                }
                            }
                        } else {
                            warn!("UNEXPECTED TARGET {} in message {}",
                                  target,
                                  format!("{}", message).trim());
                        }
                    }
                }
            }
            Command::INVITE(ref target, ref channel) => {
                if target == server.current_nickname() {
                    // Join channels when invited.
                    server.send_join(channel).unwrap();
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
                      options: &HashMap<String, String>,
                      this_channel_data: Option<&mut ChannelData>,
                      command: &str,
                      response_target: &str,
                      response_is_action: bool,
                      response_username: Option<&str>) {

    let send_line = |response_username: Option<&str>, line: &str| {
        let line_with_nick = match response_username {
            None => String::from(line),
            Some(username) => String::from(username) + ", " + line,
        };
        let adjusted_line = if response_is_action {
            format!("\x01ACTION {}\x01", line_with_nick)
        } else {
            line_with_nick
        };
        server
            .send_privmsg(response_target, &adjusted_line)
            .unwrap();
    };

    if command == "help" {
        send_line(response_username, "The commands I understand are:");
        send_line(None, "  help     Send this message.");
        send_line(None, "  intro    Send a message describing what I do.");
        send_line(None,
                  "  bye      Leave the channel.  (You can /invite me back.)");
        return;
    }

    if command == "intro" {
        let config = server.config();
        send_line(None,
                  "My job is to leave comments in github when the group discusses github issues and takes minutes in IRC.");
        send_line(None,
                  "I separate discussions by the \"Topic:\" lines, and I know what github issues to use only by lines of the form \"Topic github: <url>\", or if the \"Topic:\" line is a github URL.");
        send_line(None,
                  &*format!("I'm only allowed to comment on issues in the repositories: {}.",
                            options["github_repos_allowed"]));
        let owners = if let Some(v) = config.owners.as_ref() {
            v.join(" ")
        } else {
            String::from("")
        };
        send_line(None,
                  &*format!("My source code is at {} and I'm run by {}.",
                            options["source"],
                            owners));
        return;
    }

    if command == "bye" {
        match this_channel_data {
            None => {
                send_line(response_username, "'bye' only works in a channel");
            }
            Some(this_channel_data) => {
                this_channel_data.end_topic();
                server
                    .send(Command::PART(String::from(response_target),
                                        Some(format!("Leaving at request of {}.  Feel free to /invite me back.",
                                                     response_username.unwrap()))))
                    .unwrap();
            }
        }
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
    resolutions: Vec<String>,
}

struct ChannelData<'opts> {
    current_topic: Option<TopicData>,
    options: &'opts HashMap<String, String>,
}

impl fmt::Display for ChannelLine {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.is_action {
            write!(f, "* {} {}", self.source, self.message)
        } else {
            write!(f, "<{}> {}", self.source, self.message)
        }
    }
}

impl TopicData {
    fn new(topic: &str) -> TopicData {
        let topic_ = String::from(topic);
        TopicData {
            topic: topic_,
            github_url: None,
            lines: vec![],
            resolutions: vec![],
        }
    }
}

impl fmt::Display for TopicData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.resolutions.len() == 0 {
            try!(write!(f,
                        "The CSS Working Group just discussed {}.\n\n",
                        self.topic));
        } else {
            try!(write!(f,
                        "The CSS Working Group just discussed {}, and agreed to the following resolutions:\n\n",
                        self.topic));
            for resolution in &self.resolutions {
                try!(write!(f, "```\n{}\n```\n\n", resolution));
            }
        }

        try!(write!(f,
                    "<details><summary>The full IRC log of that discussion</summary>\n"));
        try!(write!(f, "\n```\n"));
        for line in &self.lines {
            try!(write!(f, "{}\n", line));
        }
        try!(write!(f, "```\n</details>\n"));
        Ok(())
    }
}

/// A case-insensitive version of starts_with.
fn ci_starts_with(s: &str, prefix: &str) -> bool {
    assert!(prefix.to_lowercase() == prefix);

    s.len() >= prefix.len() && s[0..prefix.len()].to_lowercase() == prefix
}

/// Remove a case-insensitive start of the line, and if that prefix is
/// present return the rest of the line.
fn strip_ci_prefix(s: &str, prefix: &str) -> Option<String> {
    if ci_starts_with(s, prefix) {
        Some(String::from(s[prefix.len()..].trim_left()))
    } else {
        None
    }
}

impl<'opts> ChannelData<'opts> {
    fn new(options_: &'opts HashMap<String, String>) -> ChannelData {
        ChannelData {
            current_topic: None,
            options: options_,
        }
    }

    // Returns the response that should be sent to the message over IRC.
    fn add_line(&mut self, line: ChannelLine) -> Option<String> {
        if let Some(ref topic) = strip_ci_prefix(&line.message, "topic:") {
            self.start_topic(topic);
        }
        if line.source == "trackbot" && line.is_action == true &&
           line.message == "is ending a teleconference." {
            self.end_topic();
        }
        match self.current_topic {
            None => None,
            Some(ref mut data) => {
                let new_url_option = extract_github_url(&line.message, self.options);
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

                if line.message.starts_with("RESOLUTION") || line.message.starts_with("RESOLVED") {
                    data.resolutions.push(line.message.clone());
                }

                data.lines.push(line);

                response
            }
        }
    }

    fn start_topic(&mut self, topic: &str) {
        self.end_topic();
        self.current_topic = Some(TopicData::new(topic));
    }

    fn end_topic(&mut self) {
        // TODO: Test the topic boundary code.
        if let Some(topic) = self.current_topic.take() {
            if topic.github_url.is_some() {
                let task = GithubCommentTask::new(topic, self.options);
                task.run();
            }
        }
    }
}

fn extract_github_url(message: &str, options: &HashMap<String, String>) -> Option<String> {
    lazy_static! {
        static ref GITHUB_URL_RE: Regex =
            Regex::new(r"^https://github.com/(?P<repo>[^/]*/[^/]*)/issues/(?P<number>[0-9]+)$")
            .unwrap();
    }
    let ref allowed_repos = options["github_repos_allowed"];
    for prefix in ["topic:", "github topic:"].into_iter() {
        if let Some(ref maybe_url) = strip_ci_prefix(&message, prefix) {
            if let Some(ref caps) = GITHUB_URL_RE.captures(maybe_url) {
                for repo in allowed_repos.split_whitespace() {
                    if caps["repo"] == *repo {
                        return Some(maybe_url.clone());
                    }
                }
            }
        }
    }
    None
}

struct GithubCommentTask {
    data: TopicData,
    github: Github,
}

impl GithubCommentTask {
    fn new(data_: TopicData, options: &HashMap<String, String>) -> GithubCommentTask {
        let github_ =
            Github::new(&*options["github_uastring"],
                        Client::with_connector(HttpsConnector::new(NativeTlsClient::new()
                                                                       .unwrap())),
                        Credentials::Token(options["github_access_token"].clone()));
        GithubCommentTask {
            data: data_,
            github: github_,
        }
    }
    fn run(self) {
        thread::spawn(move || { self.main(); });
    }
    fn main(&self) {
        lazy_static! {
            static ref GITHUB_URL_RE: Regex =
                Regex::new(r"^https://github.com/(?P<owner>[^/]*)/(?P<repo>[^/]*)/issues/(?P<number>[0-9]+)$")
                .unwrap();
        }

        if let Some(ref github_url) = self.data.github_url {
            if let Some(ref caps) = GITHUB_URL_RE.captures(github_url) {
                let repo = self.github
                    .repo(String::from(&caps["owner"]), String::from(&caps["repo"]));
                let issue = repo.issue(caps["number"].parse::<u64>().unwrap());
                let comments = issue.comments();

                let comment_text = format!("{}", self.data);
                comments
                    .create(&CommentOptions { body: comment_text })
                    .unwrap();
            } else {
                warn!("How does {} fail to match now when it matched before?",
                      github_url)
            }
        }
    }
}
