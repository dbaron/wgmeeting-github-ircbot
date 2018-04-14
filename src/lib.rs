// see 'rustc -W help'
#![warn(missing_docs, unused_extern_crates, unused_results)]

//! An IRC bot that posts comments to github when W3C-style IRC minuting is
//! combined with "Github:", "Github topic:", or "Github issue:" lines that
//! give the github issue to comment in.

extern crate futures;
extern crate hubcaps;
extern crate hyper;
extern crate hyper_tls;
extern crate irc;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate regex;
extern crate tokio_core;

use futures::prelude::*;
use futures::future::ok;
use hyper::client::HttpConnector;
use hyper_tls::HttpsConnector;
use hubcaps::{Credentials, Github};
use hubcaps::comments::CommentOptions;
use irc::client::Client;
use irc::client::ext::ClientExt;
use irc::client::prelude::{Command, IrcClient};
use irc::proto::message::Message;
use tokio_core::reactor::Handle;
use regex::Regex;
use std::cmp;
use std::collections::HashMap;
use std::fmt;
use std::iter;

#[derive(Copy, Clone)]
/// Whether to use a real github connection for real use of the bot, or a fake
/// one for testing.
pub enum GithubType {
    /// Use a real github connection for operating the bot.
    RealGithubConnection,
    /// Don't make real connections to github (for tests).
    MockGithubConnection,
}

/// Run an iteration of the main loop of the bot, given an IRC server
/// (with a real or mock / connection).
pub fn process_irc_message<'opts>(
    irc: &IrcClient,
    irc_state: &mut IRCState<'opts>,
    options: &'opts HashMap<String, String>,
    message: Message,
) {
    match message.command {
        Command::PRIVMSG(ref target, ref msg) => {
            match message.source_nickname() {
                None => {
                    warn!(
                        "PRIVMSG without a source! {}",
                        format!("{}", message).trim()
                    );
                }
                Some(ref source) => {
                    let source_ = String::from(*source);
                    let line = if msg.starts_with("\x01ACTION ") && msg.ends_with("\x01") {
                        ChannelLine {
                            source: source_,
                            is_action: true,
                            message: filter_bot_hidden(&msg[8..msg.len() - 1]),
                        }
                    } else {
                        ChannelLine {
                            source: source_,
                            is_action: false,
                            message: filter_bot_hidden(msg),
                        }
                    };
                    let mynick = irc.current_nickname();
                    if target == mynick {
                        // An actual private message.
                        info!("[{}] {}", source, line);
                        handle_bot_command(
                            &irc,
                            options,
                            irc_state,
                            &line.message,
                            source,
                            false,
                            None,
                        )
                    } else if target.starts_with('#') {
                        // A message in a channel.
                        info!("[{}] {}", target, line);
                        match check_command_in_channel(mynick, &line.message) {
                            Some(ref command) => handle_bot_command(
                                &irc,
                                options,
                                irc_state,
                                command,
                                target,
                                line.is_action,
                                Some(source),
                            ),
                            None => {
                                if !is_present_plus(&*line.message) {
                                    // FIXME: refactor away clone
                                    let event_loop = irc_state.event_loop.clone();
                                    let this_channel_data = irc_state.channel_data(target, options);
                                    if let Some(response) =
                                        this_channel_data.add_line(&irc, event_loop, line)
                                    {
                                        send_irc_line(&irc, target, true, response);
                                    }
                                }
                            }
                        }
                    } else {
                        warn!(
                            "UNEXPECTED TARGET {} in message {}",
                            target,
                            format!("{}", message).trim()
                        );
                    }
                }
            }
        }
        Command::INVITE(ref target, ref channel) => {
            if target == irc.current_nickname() {
                // Join channels when invited.
                irc.send_join(channel).unwrap();
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
        Some(index) => String::from(&line[..index]) + "[hidden]",
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
            bytes[..present_plus.len() + 1].eq_ignore_ascii_case("present+ ".as_bytes())
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

fn send_irc_line(irc: &IrcClient, target: &str, is_action: bool, line: String) {
    // We can't send an IRC message longer than 512 characters.  This includes
    // the "PRIVMSG" and the spaces between the parts.  If we fail to do this,
    // the server might disconnect us with "Request too long", or for messages
    // slightly under the longer threshold, might cut the ends of the messages
    // when sending them onwards to other clients.
    // Test the loop condition at the end so we transmit an empty line if
    // given one.  (This is important at least for the tests, which use IRC
    // messages to simulate the github comments.  It probably isn't important
    // for anything else.)
    let max_length = 463 - 8 - target.len() - (if is_action { 9 } else { 0 });
    let mut segment_start = 0;
    loop {
        let segment_end = if line.len() - segment_start <= max_length {
            line.len()
        } else {
            let mut byte_starting_char = segment_start + max_length;
            let bytes = line.as_bytes();
            while bytes[byte_starting_char] & 0b_1100_0000_u8 == 0b_1000_0000_u8 {
                // We found a UTF-8 continuation byte, so shorten.
                byte_starting_char = byte_starting_char - 1;
            }
            byte_starting_char
        };

        let slice =
            String::from_utf8(line.as_bytes()[segment_start..segment_end].to_vec()).unwrap();

        let adjusted_slice = if is_action {
            info!("[{}] > * {}", target, slice);
            format!("\x01ACTION {}\x01", slice)
        } else {
            info!("[{}] > {}", target, slice);
            slice
        };
        irc.send_privmsg(target, &*adjusted_slice).unwrap();

        segment_start = segment_end;

        if segment_start >= line.len() {
            break;
        }
    }
}

/// Return the description used by the bot to describe its own version and
/// commit hash.  Public only because the test code needs access to it, in
/// order to expect the right string.
pub fn code_description() -> &'static String {
    lazy_static! {
        static ref CODE_DESCRIPTION: String =
            format!("{} version {}, compiled from {}",
                    env!("CARGO_PKG_NAME"),
                    env!("CARGO_PKG_VERSION"),
                    include_str!(concat!(env!("OUT_DIR"), "/git-hash")).trim_right());
    }
    &CODE_DESCRIPTION
}

fn handle_bot_command<'opts>(
    irc: &IrcClient,
    options: &'opts HashMap<String, String>,
    irc_state: &mut IRCState<'opts>,
    command: &str,
    response_target: &str,
    response_is_action: bool,
    response_username: Option<&str>,
) {
    let send_line = |response_username: Option<&str>, line: &str| {
        let line_with_nick = match response_username {
            None => String::from(line),
            Some(username) => String::from(username) + ", " + line,
        };
        send_irc_line(irc, response_target, response_is_action, line_with_nick);
    };

    // Remove a question mark at the end of the command if it exists
    let command_without_question_mark = if command.ends_with("?") {
        &command[..command.len() - 1]
    } else {
        command
    };

    match command_without_question_mark {
        "help" => {
            send_line(response_username, "The commands I understand are:");
            send_line(None, "  help      - Send this message.");
            send_line(None, "  intro     - Send a message describing what I do.");
            send_line(
                None,
                "  status    - Send a message with current bot status.",
            );
            send_line(
                None,
                "  bye       - Leave the channel.  (You can /invite me back.)",
            );
            send_line(
                None,
                "  end topic - End the current topic without starting a new one.",
            );
            send_line(
                None,
                "  reboot - Make me leave the server and exit.  If properly configured, I will \
                 then update myself and return.",
            );
        }
        "intro" => {
            let config = irc.config();
            send_line(
                None,
                "My job is to leave comments in github when the group discusses github issues and \
                 takes minutes in IRC.",
            );
            send_line(
                None,
                "I separate discussions by the \"Topic:\" lines, and I know what github issues to \
                 use only by lines of the form \"GitHub: <url> | none\".",
            );
            send_line(
                None,
                &*format!(
                    "I'm only allowed to comment on issues in the repositories: {}.",
                    options["github_repos_allowed"]
                ),
            );
            let owners = if let Some(v) = config.owners.as_ref() {
                v.join(" ")
            } else {
                String::from("")
            };
            send_line(
                None,
                &*format!(
                    "My source code is at {} and I'm run by {}.",
                    options["source"], owners
                ),
            );
        }
        "status" => {
            send_line(
                response_username,
                &*format!(
                    "This is {}, which is probably in the repository at \
                     https://github.com/dbaron/wgmeeting-github-ircbot/",
                    &*code_description()
                ),
            );
            send_line(None, "I currently have data for the following channels:");
            let mut sorted_channels: Vec<&String> = irc_state.channel_data.keys().collect();
            sorted_channels.sort();
            for channel in sorted_channels {
                let ref channel_data = irc_state.channel_data[channel];
                if let Some(ref topic) = channel_data.current_topic {
                    send_line(
                        None,
                        &*format!(
                            "  {} ({} lines buffered on \"{}\")",
                            channel,
                            topic.lines.len(),
                            topic.topic
                        ),
                    );
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
                let event_loop = irc_state.event_loop.clone(); // FIXME: refactor away clone
                let this_channel_data = irc_state.channel_data(response_target, options);
                this_channel_data.end_topic(irc, event_loop);
                irc.send(Command::PART(
                    String::from(response_target),
                    Some(format!(
                        "Leaving at request of {}.  Feel free to /invite me back.",
                        response_username.unwrap()
                    )),
                )).unwrap();
            } else {
                send_line(response_username, "'bye' only works in a channel");
            }
        }
        "end topic" => {
            if response_target.starts_with('#') {
                let event_loop = irc_state.event_loop.clone(); // FIXME: refactor away clone
                let this_channel_data = irc_state.channel_data(response_target, options);
                this_channel_data.end_topic(irc, event_loop);
            } else {
                send_line(response_username, "'end topic' only works in a channel");
            }
        }
        "reboot" => {
            let mut channels_with_topics = irc_state
                .channel_data
                .iter()
                .filter_map(|(channel, channel_data)| {
                    if channel_data.current_topic.is_some() {
                        Some(channel)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            if channels_with_topics.is_empty() {
                // quit from the server, with a message
                irc.send(Command::QUIT(Some(format!(
                    "{}, rebooting at request of {}.",
                    &*code_description(),
                    response_username.unwrap()
                )))).unwrap();

                // exit, and assume whatever started the bot will restart it
                unimplemented!(); // This will exit.  Maybe do something cleaner later?
            } else {
                // refuse to reboot
                channels_with_topics.sort();
                send_line(
                    response_username,
                    &*format!(
                        "Sorry, I can't reboot right now because I have buffered topics in{}.",
                        channels_with_topics
                            .iter()
                            .flat_map(|s| " ".chars().chain(s.chars()))
                            .collect::<String>()
                    ),
                );
            }
        }
        _ => {
            send_line(
                response_username,
                "Sorry, I don't understand that command.  Try 'help'.",
            );
        }
    }
}

/// The data from IRC channels that we're storing in order to make comments in
/// github.
pub struct IRCState<'opts> {
    channel_data: HashMap<String, ChannelData<'opts>>,
    github_type: GithubType,
    event_loop: Handle,
}

impl<'opts> IRCState<'opts> {
    /// Create an empty IRCState.
    pub fn new(github_type_: GithubType, event_loop_: &Handle) -> IRCState<'opts> {
        IRCState {
            channel_data: HashMap::new(),
            github_type: github_type_,
            event_loop: event_loop_.clone(),
        }
    }

    fn channel_data(
        &mut self,
        channel: &str,
        options: &'opts HashMap<String, String>,
    ) -> &mut ChannelData<'opts> {
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
    let (cur, max) = s.chars().fold((0, 0), |(cur, max), char| {
        if char == '`' {
            (cur + 1, max)
        } else {
            (0, cmp::max(cur, max))
        }
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
    format!(
        "{}{}{}{}{}",
        tick_string, space_first, s, space_last, tick_string
    )
}

fn escape_for_html_block(s: &str) -> String {
    // Insert a zero width no-break space (U+FEFF, also byte order mark) between
    // word-starting-# and a digit, so that github doesn't linkify things like "#1"
    // into links to github issues.
    //
    // Do this first, in case we later start doing escaping that produces HTML
    // numeric character references in decimal.
    lazy_static! {
        static ref ISSUE_RE: Regex =
            Regex::new(r"(?P<space>[[:space:]])[#](?P<number>[0-9])")
            .unwrap();
    }
    let no_issue_links = ISSUE_RE.replace_all(s, "${space}#\u{feff}${number}");

    no_issue_links.replace("&", "&amp;").replace("<", "&lt;")
}

impl fmt::Display for TopicData {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Use `...` around the topic and resolutions, and ```-escaping around
        // the IRC log to avoid most concern about escaping.
        if self.resolutions.len() == 0 {
            try!(write!(
                f,
                "The Working Group just discussed {}.\n",
                if self.topic == "" {
                    String::from("this issue")
                } else {
                    escape_as_code_span(&*self.topic)
                }
            ));
        } else {
            try!(write!(
                f,
                "The Working Group just discussed {}, and agreed to the \
                 following resolutions:\n\n",
                escape_as_code_span(&*self.topic)
            ));
            for resolution in &self.resolutions {
                try!(write!(f, "* {}\n", escape_as_code_span(&*resolution)));
            }
        }

        try!(write!(
            f,
            "\n<details><summary>The full IRC log of that \
             discussion</summary>\n"
        ));
        for line in &self.lines {
            try!(write!(
                f,
                "{}<br>\n",
                escape_for_html_block(&*format!("{}", line))
            ));
        }
        try!(write!(f, "</details>\n"));
        Ok(())
    }
}

/// A case-insensitive version of starts_with.
fn ci_starts_with(s: &str, prefix: &str) -> bool {
    debug_assert!(prefix.to_lowercase() == prefix);
    debug_assert!(prefix.len() == prefix.chars().count());

    s.len() >= prefix.len()
        && prefix
            .as_bytes()
            .eq_ignore_ascii_case(&s.as_bytes()[..prefix.len()])
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

/// Remove a case-insensitive start of the line (given multiple options
/// for what that start is), and if that prefix is present return the
/// rest of the line.
fn strip_one_ci_prefix<'a, T>(s: &str, prefixes: T) -> Option<String>
where
    T: Iterator<Item = &'a &'a str>,
{
    prefixes
        .filter_map(|prefix| strip_ci_prefix(s, &prefix))
        .next()
}

impl<'opts> ChannelData<'opts> {
    fn new(
        channel_name_: &str,
        options_: &'opts HashMap<String, String>,
        github_type_: GithubType,
    ) -> ChannelData<'opts> {
        ChannelData {
            channel_name: String::from(channel_name_),
            current_topic: None,
            options: options_,
            github_type: github_type_,
        }
    }

    // Returns the response that should be sent to the message over IRC.
    // FIXME: Move this to be a method on IRCState.
    fn add_line(
        &mut self,
        irc: &IrcClient,
        event_loop: Handle,
        line: ChannelLine,
    ) -> Option<String> {
        match line.is_action {
            false => if let Some(ref topic) = strip_ci_prefix(&line.message, "topic:") {
                self.start_topic(irc, event_loop, topic);
            },
            true => if line.source == "trackbot" && line.message == "is ending a teleconference." {
                self.end_topic(irc, event_loop);
            },
        };
        match self.current_topic {
            None => match extract_github_url(&line.message, self.options, &None, false) {
                (Some(_), None) => Some(String::from(
                    "I can't set a github URL because you haven't started a \
                     topic.",
                )),
                (None, Some(ref extract_response)) => Some(
                    String::from(
                        "I can't set a github URL because you haven't started a topic.  \
                         Also, ",
                    ) + extract_response,
                ),
                (None, None) => None,
                _ => panic!("unexpected state"),
            },
            Some(ref mut data) => {
                let (new_url_option, extract_failure_response) =
                    extract_github_url(&line.message, self.options, &data.github_url, true);
                let response = match (new_url_option.as_ref(), &data.github_url) {
                    (None, _) => extract_failure_response,
                    (Some(&None), &None) => None,
                    (Some(&None), _) => {
                        Some(String::from("OK, I won't post this discussion to GitHub."))
                    }
                    (Some(&Some(ref new_url)), &None) => {
                        Some(format!("OK, I'll post this discussion to {}.", new_url))
                    }
                    (Some(new_url), old_url) if *old_url == *new_url => None,
                    (Some(&Some(ref new_url)), &Some(ref old_url)) => Some(format!(
                        "OK, I'll post this discussion to {} instead of {} like \
                         you said before.",
                        new_url, old_url
                    )),
                };

                if let Some(new_url) = new_url_option {
                    data.github_url = new_url;
                }

                if !line.is_action {
                    if line.message.starts_with("RESOLUTION")
                        || line.message.starts_with("RESOLVED")
                        || line.message.starts_with("SUMMARY")
                    {
                        data.resolutions.push(line.message.clone());
                    }

                    data.lines.push(line);
                }

                response
            }
        }
    }

    // FIXME: Move this to be a method on IRCState.
    fn start_topic(&mut self, irc: &IrcClient, event_loop: Handle, topic: &str) {
        self.end_topic(irc, event_loop);
        self.current_topic = Some(TopicData::new(topic));
    }

    // FIXME: Move this to be a method on IRCState.
    fn end_topic(&mut self, irc: &IrcClient, event_loop: Handle) {
        // TODO: Test the topic boundary code.
        if let Some(topic) = self.current_topic.take() {
            if topic.github_url.is_some() {
                let task = GithubCommentTask::new(
                    irc,
                    event_loop,
                    &*self.channel_name,
                    topic,
                    self.options,
                    self.github_type,
                );
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
fn extract_github_url(
    message: &str,
    options: &HashMap<String, String>,
    current_github_url: &Option<String>,
    in_topic: bool,
) -> (Option<Option<String>>, Option<String>) {
    lazy_static! {
        static ref GITHUB_URL_WHOLE_RE: Regex =
            Regex::new(r"^(?P<issueurl>https://github.com/(?P<repo>[^/]*/[^/]*)/(issues|pull)/(?P<number>[0-9]+))([#][^ ]*)?$")
            .unwrap();
        static ref GITHUB_URL_PART_RE: Regex =
            Regex::new(r"https://github.com/(?P<repo>[^/]*/[^/]*)/(issues|pull)/(?P<number>[0-9]+)")
            .unwrap();
    }
    let ref allowed_repos = options["github_repos_allowed"];
    if let Some(ref maybe_url) = strip_one_ci_prefix(
        &message,
        ["github:", "github topic:", "github issue:"].into_iter(),
    ) {
        if maybe_url.to_lowercase() == "none" {
            (Some(None), None)
        } else if let Some(ref caps) = GITHUB_URL_WHOLE_RE.captures(maybe_url) {
            if allowed_repos
                .split_whitespace()
                .collect::<Vec<_>>()
                .contains(&&caps["repo"])
            {
                (Some(Some(String::from(&caps["issueurl"]))), None)
            } else {
                (
                    None,
                    Some(format!(
                        "I can't comment on that github issue because it's not in \
                         a repository I'm allowed to comment on, which are: {}.",
                        allowed_repos
                    )),
                )
            }
        } else {
            (
                None,
                Some(String::from(
                    "I can't comment on that because it doesn't look like a \
                     github issue to me.",
                )),
            )
        }
    } else {
        if let Some(ref rematch) = GITHUB_URL_PART_RE.find(message) {
            if &Some(String::from(rematch.as_str())) == current_github_url || !in_topic {
                (None, None)
            } else {
                (
                    None,
                    Some(String::from(
                        "Because I don't want to spam github issues unnecessarily, \
                         I won't comment in that github issue unless you write \
                         \"Github: <issue-url> | none\" (or \"Github issue: \
                         ...\"/\"Github topic: ...\").",
                    )),
                )
            }
        } else {
            (None, None)
        }
    }
}

struct GithubCommentTask {
    // a clone of the IRCServer is OK, because it reference-counts almost all of its internals
    irc: IrcClient,
    response_target: String,
    data: TopicData,
    github: Option<Github<HttpsConnector<HttpConnector>>>, /* None means we're mocking the
                                                            * connection */
    event_loop: Handle,
}

impl GithubCommentTask {
    fn new(
        irc_: &IrcClient,
        event_loop_: Handle,
        response_target_: &str,
        data_: TopicData,
        options: &HashMap<String, String>,
        github_type_: GithubType,
    ) -> GithubCommentTask {
        let github_ = match github_type_ {
            GithubType::RealGithubConnection => Some(Github::new(
                &*options["github_uastring"],
                Some(Credentials::Token(options["github_access_token"].clone())),
                &event_loop_,
            )),
            GithubType::MockGithubConnection => None,
        };
        GithubCommentTask {
            irc: irc_.clone(),
            response_target: String::from(response_target_),
            data: data_,
            github: github_,
            event_loop: event_loop_,
        }
    }

    fn run(self) {
        // FIXME: do this again?
        // For real github connections, run on another thread, but for fake
        // ones, run synchronously to make testing easier.

        lazy_static! {
            static ref GITHUB_URL_RE: Regex =
                Regex::new(r"^https://github.com/(?P<owner>[^/]*)/(?P<repo>[^/]*)/(?P<type>(issues|pull))/(?P<number>[0-9]+)$")
                .unwrap();
        }

        if let Some(ref github_url) = self.data.github_url {
            if let Some(ref caps) = GITHUB_URL_RE.captures(github_url) {
                let comment_text = format!("{}", self.data);

                let send_response_irc = self.irc.clone();
                let send_response_target = self.response_target.clone();
                let send_response = move |response: String| {
                    send_irc_line(&send_response_irc, &*send_response_target, true, response);
                };
                match self.github {
                    Some(ref github) => {
                        let repo =
                            github.repo(String::from(&caps["owner"]), String::from(&caps["repo"]));
                        let num = caps["number"].parse::<u64>().unwrap();
                        // FIXME: share this better (without making the
                        // borrow checker object)!
                        let commentopts = &CommentOptions { body: comment_text };
                        let github_url_for_response = github_url.clone();
                        let comment_task = match &(caps["type"]) {
                            "issues" => repo.issue(num).comments().create(commentopts),
                            "pull" => repo.pulls().get(num).comments().create(commentopts),
                            _ => panic!("the regexp should not have allowed this"),
                        }.then(move |result| {
                            ok::<String, ()>(match result {
                                Ok(_) => {
                                    format!("Successfully commented on {}", github_url_for_response)
                                }
                                Err(err) => format!(
                                    /* FIXME: Remove newlines *and backtrace* from err. */ "UNABLE TO COMMENT on {} due to error: {:?}",
                                    github_url_for_response, err
                                ),
                            })
                        });

                        let mut label_tasks = Vec::new();
                        if self.data.resolutions.len() > 0 && &(caps["type"]) == "issues" {
                            // We had resolutions, so remove the "Agenda+" and
                            // "Agenda+ F2F" tags, if present.
                            // FIXME: Do this for pulls too, once
                            // hubcaps gives access to labels on a pull
                            // request.

                            // Explicitly discard any errors.  That's because
                            // this might give an error if the label isn't
                            // present.
                            // FIXME:  But it might also give a (different)
                            // error if we don't have write access to the
                            // repository, so we really ought to distinguish,
                            // and report the latter.
                            let issue = repo.issue(num);
                            let labels = issue.labels();
                            for label in ["Agenda+", "Agenda+ F2F"].into_iter() {
                                let success_str = format!(" and removed the \"{}\" label", label);
                                label_tasks.push(labels.remove(label).then(|result| {
                                    ok(match result {
                                        Ok(_) => success_str,
                                        Err(_) => String::from(""),
                                    })
                                }));
                            }
                        }

                        self.event_loop.spawn(
                            comment_task
                                .join(futures::future::join_all(label_tasks))
                                .map(|(comment_msg, label_msg_vec)| {
                                    iter::once(&comment_msg)
                                        .chain(label_msg_vec.iter())
                                        .flat_map(|s| s.chars())
                                        .collect::<String>()
                                })
                                .map(move |s| send_response(s)),
                        );
                    }
                    None => {
                        // Mock the github comments by sending them over IRC
                        // to a fake user called github-comments.
                        let send_github_comment_line = |line: &str| {
                            send_irc_line(&self.irc, "github-comments", false, String::from(line))
                        };
                        send_github_comment_line(
                            format!("!BEGIN GITHUB COMMENT IN {}", github_url).as_str(),
                        );
                        for line in comment_text.split('\n') {
                            send_github_comment_line(line);
                        }
                        send_github_comment_line(
                            format!("!END GITHUB COMMENT IN {}", github_url).as_str(),
                        );
                        send_response(format!("{} on {}", "Successfully commented", github_url));
                    }
                };
            } else {
                warn!(
                    "How does {} fail to match now when it matched before?",
                    github_url
                )
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
        assert_eq!(
            strip_ci_prefix("Topic:hello", "topic:"),
            Some(String::from("hello"))
        );
        assert_eq!(
            strip_ci_prefix("Topic: hello", "topic:"),
            Some(String::from("hello"))
        );
        assert_eq!(
            strip_ci_prefix("topic: hello", "topic:"),
            Some(String::from("hello"))
        );
        assert_eq!(strip_ci_prefix("Issue: hello", "topic:"), None);
        assert_eq!(strip_ci_prefix("Topic: hello", "issue:"), None);
        assert_eq!(strip_ci_prefix("Github topic: hello", "topic:"), None);
    }

    #[test]
    fn test_strip_one_ci_prefix() {
        assert_eq!(
            strip_one_ci_prefix("GitHub:url goes here", ["issue:", "github:"].into_iter()),
            Some(String::from("url goes here"))
        );
        assert_eq!(
            strip_one_ci_prefix("GITHUB: url goes here", ["issue:", "github:"].into_iter()),
            Some(String::from("url goes here"))
        );
        assert_eq!(
            strip_one_ci_prefix("issue: url goes here", ["issue:", "github:"].into_iter()),
            Some(String::from("url goes here"))
        );
        assert_eq!(
            strip_one_ci_prefix("topic: url goes here", ["issue:", "github:"].into_iter()),
            None
        );
    }
}
