// see 'rustc -W help'
#![warn(missing_docs, unused_extern_crates, unused_results)]

//! An IRC bot that posts comments to github when W3C-style IRC minuting is
//! combined with "Github topic:" or "Github issue:" lines that give the
//! github issue to comment in.

#[macro_use]
extern crate log;
extern crate irc;
#[macro_use]
extern crate lazy_static;
extern crate regex;
extern crate hyper;
extern crate hubcaps;
extern crate hyper_native_tls;

use hubcaps::{Credentials, Github};
use hubcaps::comments::CommentOptions;
use hyper::Client;
use hyper::net::HttpsConnector;
use hyper_native_tls::NativeTlsClient;
use irc::client::data::command::Command;
use irc::client::prelude::*;
use regex::Regex;
use std::ascii::AsciiExt;
use std::cmp;
use std::collections::HashMap;
use std::fmt;
use std::thread;

#[derive(Copy, Clone)]
/// Whether to use a real github connection for real use of the bot, or a fake
/// one for testing.
pub enum GithubType {
    /// Use a real github connection for operating the bot.
    RealGithubConnection,
    /// Don't make real connections to github (for tests).
    MockGithubConnection,
}

/// Run the main loop of the bot, given an IRC server (with a real or mock
/// connection).
pub fn main_loop_iteration<'opts>(server: IrcServer,
                                  irc_state: &mut IRCState<'opts>,
                                  options: &'opts HashMap<String, String>,
                                  message: &Message) {
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
                            message: filter_bot_hidden(&msg[8 .. msg.len() - 1]),
                        }
                    } else {
                        ChannelLine {
                            source: source_,
                            is_action: false,
                            message: filter_bot_hidden(msg),
                        }
                    };
                    let mynick = server.current_nickname();
                    if target == mynick {
                        // An actual private message.
                        info!("[{}] {}", source, line);
                        handle_bot_command(&server,
                                           options,
                                           irc_state,
                                           &line.message,
                                           source,
                                           false,
                                           None)
                    } else if target.starts_with('#') {
                        // A message in a channel.
                        info!("[{}] {}", target, line);
                        match check_command_in_channel(mynick, &line.message) {
                            Some(ref command) => {
                                handle_bot_command(&server,
                                                   options,
                                                   irc_state,
                                                   command,
                                                   target,
                                                   line.is_action,
                                                   Some(source))
                            }
                            None => {
                                if !is_present_plus(&*line.message) {
                                    let this_channel_data = irc_state.channel_data(target, options);
                                    if let Some(response) =
                                        this_channel_data.add_line(&server, line) {
                                        send_irc_line(&server, target, true, response);
                                    }
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

/// Remove anything in a line that is after [off] to prevent it from being
/// logged, to match the convention of other W3C logging bots.
fn filter_bot_hidden(line: &str) -> String {
    match line.find("[off]") {
        None => String::from(line),
        Some(index) => String::from(&line[.. index]) + "[hidden]",
    }
}

// Is this message either case-insensitively "Present+" or something that
// begins with "Present+ " (with space)?
fn is_present_plus(line: &str) -> bool {
    let bytes = line.as_bytes();
    let present_plus = "present+".as_bytes();
    match bytes.len().cmp(&present_plus.len()) {
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => bytes.eq_ignore_ascii_case(present_plus),
        std::cmp::Ordering::Greater => {
            bytes[.. present_plus.len() + 1].eq_ignore_ascii_case("present+ ".as_bytes())
        }
    }
}

// Take a message in the channel, and see if it was a message sent to
// this bot.
fn check_command_in_channel(mynick: &str, msg: &String) -> Option<String> {
    if !msg.starts_with(mynick) {
        return None;
    }
    let after_nick = &msg[mynick.len() ..];
    if !after_nick.starts_with(":") && !after_nick.starts_with(",") {
        return None;
    }
    let after_punct = &after_nick[1 ..];
    Some(String::from(after_punct.trim_left()))
}

fn send_irc_line(server: &IrcServer, target: &str, is_action: bool, line: String) {
    let adjusted_line = if is_action {
        format!("\x01ACTION {}\x01", line)
    } else {
        line
    };
    server.send_privmsg(target, &*adjusted_line).unwrap();
}

fn handle_bot_command<'opts>(server: &IrcServer,
                             options: &'opts HashMap<String, String>,
                             irc_state: &mut IRCState<'opts>,
                             command: &str,
                             response_target: &str,
                             response_is_action: bool,
                             response_username: Option<&str>) {

    let send_line = |response_username: Option<&str>, line: &str| {
        let line_with_nick = match response_username {
            None => String::from(line),
            Some(username) => String::from(username) + ", " + line,
        };
        send_irc_line(server, response_target, response_is_action, line_with_nick);
    };

    // Remove a question mark at the end of the command if it exists
    let command_without_question_mark = if command.ends_with("?") {
        &command[.. command.len() - 1]
    } else {
        command
    };

    match command_without_question_mark {
        "help" => {
            send_line(response_username, "The commands I understand are:");
            send_line(None, "  help      - Send this message.");
            send_line(None, "  intro     - Send a message describing what I do.");
            send_line(None,
                      "  status    - Send a message with current bot \
                       status.");
            send_line(None,
                      "  bye       - Leave the channel.  (You can /invite \
                       me back.)");
            send_line(None,
                      "  end topic - End the current topic without \
                       starting a new one.");
        }
        "intro" => {
            let config = server.config();
            send_line(None,
                      "My job is to leave comments in github when the \
                       group discusses github issues and takes minutes in \
                       IRC.");
            send_line(None,
                      "I separate discussions by the \"Topic:\" lines, and \
                       I know what github issues to use only by lines of \
                       the form \"GitHub topic: <url> | none\".");
            send_line(None,
                      &*format!("I'm only allowed to comment on issues in \
                                 the repositories: {}.",
                                options["github_repos_allowed"]));
            let owners = if let Some(v) = config.owners.as_ref() {
                v.join(" ")
            } else {
                String::from("")
            };
            send_line(None,
                      &*format!("My source code is at {} and I'm run by \
                                 {}.",
                                options["source"],
                                owners));
        }
        "status" => {
            send_line(response_username,
                      &*format!("This is {} version {}, compiled from {} \
                                 which is probably in the repository at \
                                 https://github.\
                                 com/dbaron/wgmeeting-github-ircbot/",
                                env!("CARGO_PKG_NAME"),
                                env!("CARGO_PKG_VERSION"),
                                include_str!(concat!(env!("OUT_DIR"), "/git-hash"))));
            send_line(None, "I currently have data for the following channels:");
            let mut sorted_channels: Vec<&String> = irc_state.channel_data.keys().collect();
            sorted_channels.sort();
            for channel in sorted_channels {
                let ref channel_data = irc_state.channel_data[channel];
                if let Some(ref topic) = channel_data.current_topic {
                    send_line(None,
                              &*format!("  {} ({} lines buffered on \
                                         \"{}\")",
                                        channel,
                                        topic.lines.len(),
                                        topic.topic));
                    match topic.github_url {
                        None => send_line(None, "    no GitHub URL to comment on"),
                        Some(ref github_url) => {
                            send_line(None, &*format!("    will comment on {}", github_url))
                        }
                    };
                } else {
                    send_line(None, &*format!("  {} (no topic data buffered)", channel));
                }
            }
        }
        "bye" => {
            if response_target.starts_with('#') {
                let this_channel_data = irc_state.channel_data(response_target, options);
                this_channel_data.end_topic(server);
                server
                    .send(Command::PART(String::from(response_target),
                                        Some(format!("Leaving at \
                                                      request of {}.  \
                                                      Feel free to \
                                                      /invite me back.",
                                                     response_username.unwrap()))))
                    .unwrap();
            } else {
                send_line(response_username, "'bye' only works in a channel");
            }
        }
        "end topic" => {
            if response_target.starts_with('#') {
                let this_channel_data = irc_state.channel_data(response_target, options);
                this_channel_data.end_topic(server);
            } else {
                send_line(response_username, "'end topic' only works in a channel");
            }
        }
        _ => {
            send_line(response_username,
                      "Sorry, I don't understand that command.  Try 'help'.");
        }
    }
}

/// The data from IRC channels that we're storing in order to make comments in
/// github.
pub struct IRCState<'opts> {
    channel_data: HashMap<String, ChannelData<'opts>>,
    github_type: GithubType,
}

impl<'opts> IRCState<'opts> {
    /// Create an empty IRCState.
    pub fn new(github_type_: GithubType) -> IRCState<'opts> {
        IRCState {
            channel_data: HashMap::new(),
            github_type: github_type_,
        }
    }

    fn channel_data(&mut self,
                    channel: &str,
                    options: &'opts HashMap<String, String>)
                    -> &mut ChannelData<'opts> {
        let github_type = self.github_type;
        self.channel_data
            .entry(String::from(channel))
            .or_insert_with(|| ChannelData::new(channel, options, github_type))
    }
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
    channel_name: String,
    current_topic: Option<TopicData>,
    options: &'opts HashMap<String, String>,
    github_type: GithubType,
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

/// https://github.github.com/gfm/#code-spans describes how code spans can
/// be escaped with any number of ` characters.  This function attempts to
/// use as few as possibly by finding the maximum sequence of ` characters
/// in the text that we want to escape, and then surrounding the text by
/// one more than that number of characters.
fn escape_as_code_span(s: &str) -> String {
    // // This is simpler but potentially O(N^2), but only if people type lots
    // // of backticks.
    // let tick_count = (1..).find(|n| !s.contains("`".repeat(n)));

    // Note: max doesn't include cur.
    let (cur, max) = s.chars()
        .fold((0, 0), |(cur, max), char| if char == '`' {
            (cur + 1, max)
        } else {
            (0, cmp::max(cur, max))
        });
    let tick_count = cmp::max(cur, max) + 1;

    let tick_string = "`".repeat(tick_count);
    let backtick_byte = "`".as_bytes().first();
    let space_first = if s.as_bytes().first() == backtick_byte {
        " "
    } else {
        ""
    };
    let space_last = if s.as_bytes().last() == backtick_byte {
        " "
    } else {
        ""
    };
    format!("{}{}{}{}{}",
            tick_string,
            space_first,
            s,
            space_last,
            tick_string)
}

fn escape_for_html_block(s: &str) -> String {
    s.replace("&", "&amp;").replace("<", "&lt;")
}

impl fmt::Display for TopicData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Use `...` around the topic and resolutions, and ```-escaping around
        // the IRC log to avoid most concern about escaping.
        if self.resolutions.len() == 0 {
            try!(write!(f,
                        "The CSS Working Group just discussed {}.\n",
                        if self.topic == "" {
                            String::from("this issue")
                        } else {
                            escape_as_code_span(&*self.topic)
                        }));
        } else {
            try!(write!(f,
                        "The CSS Working Group just discussed {}, and \
                         agreed to the following resolutions:\n\n",
                        escape_as_code_span(&*self.topic)));
            for resolution in &self.resolutions {
                try!(write!(f, "* {}\n", escape_as_code_span(&*resolution)));
            }
        }

        try!(write!(f,
                    "\n<details><summary>The full IRC log of that \
                     discussion</summary>\n"));
        for line in &self.lines {
            try!(write!(f, "{}<br>\n", escape_for_html_block(&*format!("{}", line))));
        }
        try!(write!(f, "</details>\n"));
        Ok(())
    }
}

/// A case-insensitive version of starts_with.
fn ci_starts_with(s: &str, prefix: &str) -> bool {
    debug_assert!(prefix.to_lowercase() == prefix);
    debug_assert!(prefix.len() == prefix.chars().count());

    s.len() >= prefix.len() &&
    prefix
        .as_bytes()
        .eq_ignore_ascii_case(&s.as_bytes()[.. prefix.len()])
}

/// Remove a case-insensitive start of the line, and if that prefix is
/// present return the rest of the line.
fn strip_ci_prefix(s: &str, prefix: &str) -> Option<String> {
    if ci_starts_with(s, prefix) {
        Some(String::from(s[prefix.len() ..].trim_left()))
    } else {
        None
    }
}

/// Remove a case-insensitive start of the line (given multiple options
/// for what that start is), and if that prefix is present return the
/// rest of the line.
fn strip_one_ci_prefix<'a, T>(s: &str, prefixes: T) -> Option<String>
    where T: Iterator<Item = &'a &'a str>
{
    prefixes
        .filter_map(|prefix| strip_ci_prefix(s, &prefix))
        .next()
}

impl<'opts> ChannelData<'opts> {
    fn new(channel_name_: &str,
           options_: &'opts HashMap<String, String>,
           github_type_: GithubType)
           -> ChannelData<'opts> {
        ChannelData {
            channel_name: String::from(channel_name_),
            current_topic: None,
            options: options_,
            github_type: github_type_,
        }
    }

    // Returns the response that should be sent to the message over IRC.
    fn add_line(&mut self, server: &IrcServer, line: ChannelLine) -> Option<String> {
        if let Some(ref topic) = strip_ci_prefix(&line.message, "topic:") {
            self.start_topic(server, topic);
        }
        if line.source == "trackbot" && line.is_action == true &&
           line.message == "is ending a teleconference." {
            self.end_topic(server);
        }
        match self.current_topic {
            None => {
                match extract_github_url(&line.message, self.options, &None) {
                    (Some(_), None) => {
                        Some(String::from("I can't set a github URL \
                                           because you haven't started a \
                                           topic."))
                    }
                    (None, Some(ref extract_response)) => {
                        Some(String::from("I can't set a github URL \
                                           because you haven't started a \
                                           topic.  Also, ") +
                             extract_response)
                    }
                    (None, None) => None,
                    _ => panic!("unexpected state"),
                }
            }
            Some(ref mut data) => {
                let (new_url_option, extract_failure_response) =
                    extract_github_url(&line.message, self.options, &data.github_url);
                let response = match (new_url_option.as_ref(), &data.github_url) {
                    (None, _) => extract_failure_response,
                    (Some(&None), &None) => None,
                    (Some(&None), _) => {
                        Some(String::from("OK, I won't post this \
                                           discussion to GitHub."))
                    }
                    (Some(&Some(ref new_url)), &None) => {
                        Some(format!("OK, I'll post this discussion to {}.", new_url))
                    }
                    (Some(new_url), old_url) if *old_url == *new_url => None,
                    (Some(&Some(ref new_url)), &Some(ref old_url)) => {
                        Some(format!("OK, I'll post this discussion to {} \
                                      instead of {} like you said before.",
                                     new_url,
                                     old_url))
                    }
                };

                if let Some(new_url) = new_url_option {
                    data.github_url = new_url;
                }

                if !line.is_action {
                    if line.message.starts_with("RESOLUTION") ||
                       line.message.starts_with("RESOLVED") {
                        data.resolutions.push(line.message.clone());
                    }

                    data.lines.push(line);
                }

                response
            }
        }
    }

    fn start_topic(&mut self, server: &IrcServer, topic: &str) {
        self.end_topic(server);
        self.current_topic = Some(TopicData::new(topic));
    }

    fn end_topic(&mut self, server: &IrcServer) {
        // TODO: Test the topic boundary code.
        if let Some(topic) = self.current_topic.take() {
            if topic.github_url.is_some() {
                let task = GithubCommentTask::new(server,
                                                  &*self.channel_name,
                                                  topic,
                                                  self.options,
                                                  self.github_type);
                task.run();
            }
        }
    }
}

/// Return a pair where:
///  * the first item is a nested option, the outer option representing
///    whether to replace the current github URL, and the inner option
///    being part of that URL (so that we can replace to no-url)
///  * the second item being a response to send over IRC, if needed, which
///    will only be present if the first item is None
fn extract_github_url(message: &str,
                      options: &HashMap<String, String>,
                      current_github_url: &Option<String>)
                      -> (Option<Option<String>>, Option<String>) {
    lazy_static! {
        static ref GITHUB_URL_WHOLE_RE: Regex =
            Regex::new(r"^https://github.com/(?P<repo>[^/]*/[^/]*)/issues/(?P<number>[0-9]+)(?:#|$)")
            .unwrap();
        static ref GITHUB_URL_PART_RE: Regex =
            Regex::new(r"https://github.com/(?P<repo>[^/]*/[^/]*)/issues/(?P<number>[0-9]+)")
            .unwrap();
    }
    let ref allowed_repos = options["github_repos_allowed"];
    if let Some(ref maybe_url) =
        strip_one_ci_prefix(&message, ["github topic:", "github issue:"].into_iter()) {
        if maybe_url.to_lowercase() == "none" {
            (Some(None), None)
        } else if let Some(ref caps) = GITHUB_URL_WHOLE_RE.captures(maybe_url) {
            if allowed_repos
                   .split_whitespace()
                   .collect::<Vec<&str>>()
                   .contains(&&caps["repo"]) {
                (Some(Some(maybe_url.clone())), None)
            } else {
                (None,
                 Some(format!("I can't comment on that github issue \
                               because it's not in a repository I'm \
                               allowed to comment on, which are: {}.",
                              allowed_repos)))
            }
        } else {
            (None,
             Some(String::from("I can't comment on that because it \
                                doesn't look like a github issue to me.")))
        }
    } else {
        if let Some(ref rematch) = GITHUB_URL_PART_RE.find(message) {
            if &Some(String::from(rematch.as_str())) == current_github_url {
                (None, None)
            } else {
                (None,
                 Some(String::from("Because I don't want to spam github \
                                    issues unnecessarily, I won't comment \
                                    in that github issue unless you write \
                                    \"Github topic: <issue-url> | none\" \
                                    (or \"Github issue: ...\").")))
            }
        } else {
            (None, None)
        }
    }
}

struct GithubCommentTask {
    // a clone of the IRCServer is OK, because it reference-counts almost all of its internals
    server: IrcServer,
    response_target: String,
    data: TopicData,
    github: Option<Github>, // None means we're mocking the connection
}

impl GithubCommentTask {
    fn new(server_: &IrcServer,
           response_target_: &str,
           data_: TopicData,
           options: &HashMap<String, String>,
           github_type_: GithubType)
           -> GithubCommentTask {
        let github_ = match github_type_ {
            GithubType::RealGithubConnection =>
            Some(Github::new(&*options["github_uastring"],
                        Client::with_connector(HttpsConnector::new(NativeTlsClient::new()
                                                                       .unwrap())),
                        Credentials::Token(options["github_access_token"].clone()))),
            GithubType::MockGithubConnection => None,
        };
        GithubCommentTask {
            server: server_.clone(),
            response_target: String::from(response_target_),
            data: data_,
            github: github_,
        }
    }

    #[allow(unused_results)]
    fn run(self) {
        // For real github connections, run on another thread, but for fake
        // ones, run synchronously to make testing easier.
        match self.github {
            Some(_) => {
                thread::spawn(move || { self.main(); });
            }
            None => self.main(),
        }
    }

    fn main(&self) {
        lazy_static! {
            static ref GITHUB_URL_RE: Regex =
                Regex::new(r"^https://github.com/(?P<owner>[^/]*)/(?P<repo>[^/]*)/issues/(?P<number>[0-9]+)$")
                .unwrap();
        }

        if let Some(ref github_url) = self.data.github_url {
            if let Some(ref caps) = GITHUB_URL_RE.captures(github_url) {
                let comment_text = format!("{}", self.data);
                let response = match self.github {
                    Some(ref github) => {

                        let repo = github.repo(String::from(&caps["owner"]),
                                               String::from(&caps["repo"]));
                        let issue = repo.issue(caps["number"].parse::<u64>().unwrap());
                        let comments = issue.comments();

                        let err = comments.create(&CommentOptions { body: comment_text });
                        let mut response = format!("{} on {}",
                                                   if err.is_ok() {
                                                       "Successfully commented"
                                                   } else {
                                                       "UNABLE TO COMMENT"
                                                   },
                                                   github_url);

                        if self.data.resolutions.len() > 0 {
                            // We had resolutions, so remove the "Agenda+" and
                            // "Agenda+ F2F" tags, if present.

                            // Explicitly discard any errors.  That's because
                            // this
                            // might give an error if the label isn't present.
                            // FIXME:  But it might also give a (different)
                            // error if
                            // we don't have write access to the repository,
                            // so we
                            // really ought to distinguish, and report the
                            // latter.
                            let labels = issue.labels();
                            for label in ["Agenda+", "Agenda+ F2F"].into_iter() {
                                if labels.remove(label).is_ok() {
                                    response.push_str(&*format!(" and removed the \"{}\" label",
                                                                label));
                                }
                            }
                        }
                        response
                    }
                    None => {
                        // Mock the github comments by sending them over IRC
                        // to a fake user called github-comments.
                        let send_github_comment_line = |line: &str| {
                            send_irc_line(&self.server,
                                          "github-comments",
                                          false,
                                          String::from(line))
                        };
                        send_github_comment_line(format!("!BEGIN GITHUB COMMENT IN {}",
                                                         github_url)
                                                         .as_str());
                        for line in comment_text.split('\n') {
                            send_github_comment_line(line);
                        }
                        send_github_comment_line(format!("!END GITHUB COMMENT IN {}", github_url)
                                                     .as_str());
                        format!("{} on {}", "Successfully commented", github_url)
                    }
                };

                send_irc_line(&self.server, &*self.response_target, true, response);
            } else {
                warn!("How does {} fail to match now when it matched before?",
                      github_url)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_present_plus() {
        assert_eq!(is_present_plus("present+"), true);
        assert_eq!(is_present_plus("Present+"), true);
        assert_eq!(is_present_plus("prESeNT+"), true);
        assert_eq!(is_present_plus("present+dbaron"), false);
        assert_eq!(is_present_plus("say present+"), false);
        assert_eq!(is_present_plus("preSEnt+ dbaron"), true);
    }

    #[test]
    fn test_strip_ci_prefix() {
        assert_eq!(strip_ci_prefix("Topic:hello", "topic:"),
                   Some(String::from("hello")));
        assert_eq!(strip_ci_prefix("Topic: hello", "topic:"),
                   Some(String::from("hello")));
        assert_eq!(strip_ci_prefix("topic: hello", "topic:"),
                   Some(String::from("hello")));
        assert_eq!(strip_ci_prefix("Issue: hello", "topic:"), None);
        assert_eq!(strip_ci_prefix("Topic: hello", "issue:"), None);
        assert_eq!(strip_ci_prefix("Github topic: hello", "topic:"), None);
    }

    #[test]
    fn test_strip_one_ci_prefix() {
        assert_eq!(strip_one_ci_prefix("GitHub:url goes here", ["issue:", "github:"].into_iter()),
                   Some(String::from("url goes here")));
        assert_eq!(strip_one_ci_prefix("GITHUB: url goes here", ["issue:", "github:"].into_iter()),
                   Some(String::from("url goes here")));
        assert_eq!(strip_one_ci_prefix("issue: url goes here", ["issue:", "github:"].into_iter()),
                   Some(String::from("url goes here")));
        assert_eq!(strip_one_ci_prefix("topic: url goes here", ["issue:", "github:"].into_iter()),
                   None);
    }
}
