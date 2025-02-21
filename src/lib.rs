// see 'rustc -W help'
#![warn(
    missing_docs,
    unused,
    unused_results,
    nonstandard_style,
    rust_2018_compatibility,
    rust_2018_idioms
)]

//! An IRC bot that posts comments to github when W3C-style IRC minuting is
//! combined with "Github:", "Github topic:", or "Github issue:" lines that
//! give the github issue to comment in.

use futures::future::ok;
use futures::join;
use futures::prelude::*;
use irc::client::prelude::{Client as IrcClient, Command, Message};
use log::{info, warn};
use octorust::types::PullsUpdateReviewRequest;
use octorust::{Client as GithubClient, auth::Credentials as GithubCredentials};
use regex::Regex;
use serde::Deserialize;
use std::cmp;
use std::collections::HashMap;
use std::fmt;
use std::iter;
use std::sync::{Arc, LazyLock, RwLock};
use tokio::time::{Duration, Instant};

/// Configuration for a single IRC channel.
#[derive(Default, Deserialize)]
pub struct ChannelConfig {
    /// The name of the working group that uses this channel.
    pub group: String,
    /// GitHub repos that the bot can make comments on.
    pub github_repos_allowed: Vec<String>,
    /// Whether github comments should be resolutions only (rather than full log).
    #[serde(default)] // false
    pub publish_resolutions_only: bool,
}

/// Configuration of the bot.
#[derive(Default, Deserialize)]
pub struct BotConfig {
    /// URL of the source code repo.
    pub source: String,
    /// IRC channels the bot should join, with data about them
    #[serde(skip)]
    pub channels: HashMap<String, ChannelConfig>,
    /// UA String used for accessing GitHub.
    #[serde(default = "default_ua_string")]
    pub github_uastring: String,
    /// End activity after the given number of minutes.
    pub activity_timeout_minutes: u64,
    /// GitHub access token.
    #[serde(skip)]
    pub github_access_token: String,
    /// Bot owner IRC nicks, duplicate of what's in the IRC configuration.
    pub owners: Vec<String>,
}

fn default_ua_string() -> String {
    concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")).to_string()
}

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
pub fn process_irc_message(
    irc: &'static IrcClient,
    irc_state: &mut IRCState,
    config: &'static BotConfig,
    message: Message,
) {
    match message.command {
        Command::PRIVMSG(ref target, ref msg) => {
            match message.source_nickname() {
                None => {
                    warn!("PRIVMSG without a source! {}", format!("{message}").trim());
                }
                Some(ref source) => {
                    let source_ = String::from(*source);
                    let line = if msg.starts_with("\x01ACTION ") && msg.ends_with('\x01') {
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
                            irc,
                            config,
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
                                irc,
                                config,
                                irc_state,
                                command,
                                target,
                                line.is_action,
                                Some(source),
                            ),
                            None => {
                                if !is_present_plus(&line.message) {
                                    let mut this_channel_data =
                                        irc_state.channel_data(target, config).write().unwrap();
                                    this_channel_data.add_line(irc, target, line);
                                }
                            }
                        }

                        let this_channel_data_cell = irc_state.channel_data(target, config);
                        this_channel_data_cell.write().unwrap().last_activity = Instant::now();
                        fn create_timeout(
                            irc: &'static IrcClient,
                            /* FIXME: Why do I need (as of tokio 0.2) to use Arc and RwLock when I'm using the basic scheduler? */
                            this_channel_data_cell: Arc<RwLock<ChannelData>>,
                        ) {
                            let deadline = {
                                let mut this_channel_data = this_channel_data_cell.write().unwrap();

                                // Set |have_activity_timeout| here, separate from the
                                // computation of deadline.
                                this_channel_data.have_activity_timeout = true;

                                this_channel_data.last_activity
                                    + this_channel_data.activity_timeout_duration
                            };
                            let timeout = tokio::time::sleep_until(deadline).map({
                                let this_channel_data_cell = this_channel_data_cell;
                                move |_timeout| {
                                    {
                                        let mut this_channel_data =
                                            this_channel_data_cell.write().unwrap();
                                        this_channel_data.have_activity_timeout = false;
                                        if this_channel_data.current_topic.is_none() {
                                            // No topic to time out.
                                            return;
                                        } else if Instant::now()
                                            >= this_channel_data.last_activity
                                                + this_channel_data.activity_timeout_duration
                                        {
                                            this_channel_data.end_topic(irc);
                                            return;
                                        }
                                    }
                                    // We need to create a new timeout (outside the write
                                    // scope above, really an else on the chain inside).
                                    create_timeout(irc, this_channel_data_cell);
                                }
                            });
                            let _ = tokio::spawn(timeout);
                        }

                        let res = {
                            let this_channel_data = this_channel_data_cell.read().unwrap();
                            this_channel_data.current_topic.is_some()
                                && !this_channel_data.have_activity_timeout
                        };
                        if res {
                            create_timeout(irc, this_channel_data_cell.clone());
                        }
                    } else {
                        warn!(
                            "UNEXPECTED TARGET {} in message {}",
                            target,
                            format!("{message}").trim()
                        );
                    }
                }
            }
        }
        Command::INVITE(ref target, ref channel) => {
            if target == irc.current_nickname() && config.channels.get(channel).is_some() {
                // Join configured channels when re-invited.
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
fn check_command_in_channel(mynick: &str, msg: &str) -> Option<String> {
    if !msg.starts_with(mynick) {
        return None;
    }
    let after_nick = &msg[mynick.len()..];
    if !after_nick.starts_with(':') && !after_nick.starts_with(',') {
        return None;
    }
    let after_punct = &after_nick[1..];
    Some(String::from(after_punct.trim_start()))
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
                byte_starting_char -= 1;
            }
            byte_starting_char
        };

        let slice =
            String::from_utf8(line.as_bytes()[segment_start..segment_end].to_vec()).unwrap();

        let adjusted_slice = if is_action {
            info!("[{}] > * {}", target, slice);
            format!("\x01ACTION {slice}\x01")
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
    static CODE_DESCRIPTION: LazyLock<String> = LazyLock::new(|| {
        format!(
            "{} version {}, compiled from {}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
            include_str!(concat!(env!("OUT_DIR"), "/git-hash")).trim_end()
        )
    });
    &CODE_DESCRIPTION
}

fn handle_bot_command(
    irc: &'static IrcClient,
    config: &'static BotConfig,
    irc_state: &mut IRCState,
    command: &str,
    response_target: &str,
    response_is_action: bool,
    response_username: Option<&str>,
) {
    // send_line is a helper for sending IRC responses; it cannot
    // outlive this function.
    // FIXME: convert most callers to a send_lines() taking a vector of
    // lines, and not passing response_username every time.
    let send_line = |response_username: Option<&str>, line: &str| {
        let line_with_nick = match response_username {
            None => String::from(line),
            Some(username) => String::from(username) + ", " + line,
        };
        send_irc_line(irc, response_target, response_is_action, line_with_nick);
    };

    let take_up_check_option = {
        let (inner_command, had_take_up) =
            if let Some(take_up_argument) = strip_ci_prefix(command, "take up ") {
                (take_up_argument, true)
            } else {
                (String::from(command), false)
            };

        if let Some(subtopic_argument) = strip_ci_prefix(&inner_command, "subtopic ") {
            Some((
                subtopic_argument,
                if had_take_up {
                    "take up subtopic"
                } else {
                    "subtopic"
                },
                "Subtopic",
            ))
        } else if had_take_up {
            Some((inner_command, "take up", "Topic"))
        } else {
            strip_ci_prefix(&inner_command, "topic ")
                .map(|topic_argument| (topic_argument, "topic", "Topic"))
        }
    };
    if let Some(take_up_check_result) = take_up_check_option {
        let (take_up_url, take_up_command, topic_header) = take_up_check_result;
        if !response_target.starts_with('#') {
            send_line(
                response_username,
                &("'".to_owned() + take_up_command + "' only works in a channel"),
            );
            return;
        }

        match check_github_url(&take_up_url, config, response_target) {
            (Some(Some(ref new_url)), None) => {
                let this_channel_data_arc = irc_state.channel_data(response_target, config);
                let mut this_channel_data = this_channel_data_arc.write().unwrap();
                if let Some(ref topic) = this_channel_data.current_topic {
                    if Some(new_url) == topic.github_url.as_ref() {
                        send_line(
                            response_username,
                            &format!(
                                "ignoring request to take up {new_url} which is already the current github URL"
                            ),
                        );
                        return;
                    }
                }
                this_channel_data.end_topic(irc);

                let respond_title_future = fetch_github_title(
                    this_channel_data.config,
                    this_channel_data.github_type,
                    new_url.clone(),
                )
                .map_ok({
                    let new_url = new_url.clone();
                    let this_channel_data_arc = Arc::clone(this_channel_data_arc);
                    let response_target = String::from(response_target);
                    move |title| {
                        let mut this_channel_data = this_channel_data_arc.write().unwrap();
                        let response_target = &*response_target;

                        send_irc_line(
                            irc,
                            response_target,
                            false,
                            format!("{topic_header}: {title}"),
                        );
                        send_irc_line(
                            irc,
                            response_target,
                            response_is_action,
                            format!("OK, I'll post this discussion to {new_url}."),
                        );
                        this_channel_data.start_topic(irc, &title);
                        this_channel_data
                            .current_topic
                            .as_mut()
                            .expect("just started a topic")
                            .github_url = Some(new_url);
                    }
                });
                let _ = tokio::spawn(respond_title_future);
            }
            (None, Some(ref extract_failure_response)) => {
                send_line(response_username, extract_failure_response)
            }
            _ => panic!("unexpected state"),
        };

        return;
    }

    // Remove a question mark at the end of the command if it exists
    let command_without_question_mark = match command.strip_suffix('?') {
        Some(stripped) => stripped,
        None => command,
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
                "  reboot    - Make me leave the server and exit.  If properly configured, I will \
                 then update myself and return.",
            );
            send_line(
                None,
                "  take up [URL] - Start a new topic and print a \"Topic:\" line based on the title of the github issue/PR at URL",
            );
            send_line(
                None,
                "  topic [URL]   - Start a new topic and print a \"Topic:\" line based on the title of the github issue/PR at URL",
            );
            send_line(
                None,
                "  take up subtopic [URL] - Start a new topic and print a \"Subtopic:\" line based on the title of the github issue/PR at URL",
            );
            send_line(
                None,
                "  subtopic [URL]         - Start a new topic and print a \"Subtopic:\" line based on the title of the github issue/PR at URL",
            );
        }
        "intro" => {
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
                "You can also use the \"take up\" command if you want me to output the \"Topic:\" lines myself, based on the title of the github issue.",
            );
            if response_target.starts_with('#') {
                send_line(
                    None,
                    &format!(
                        "In this channel, I'm only allowed to comment on issues in the repositories: {:?}.",
                        config.channels[response_target].github_repos_allowed,
                    ),
                );
            }
            let owners = config.owners.join(" ");
            send_line(
                None,
                &format!(
                    "My source code is at {} and I'm run by {}.",
                    config.source, owners,
                ),
            );
        }
        "status" => {
            send_line(
                response_username,
                &format!(
                    "This is {}, which is probably in the repository at \
                     https://github.com/dbaron/wgmeeting-github-ircbot/",
                    code_description()
                ),
            );
            send_line(None, "I currently have data for the following channels:");
            let mut sorted_channels: Vec<&String> = irc_state.channel_data.keys().collect();
            sorted_channels.sort();
            for channel in sorted_channels {
                let channel_data = irc_state.channel_data[channel].read().unwrap();
                match channel_data.current_topic {
                    Some(ref topic) => {
                        send_line(
                            None,
                            &format!(
                                "  {} ({} lines buffered on \"{}\")",
                                channel,
                                topic.lines.len(),
                                topic.topic
                            ),
                        );
                        match topic.github_url {
                            None => send_line(None, "    no GitHub URL to comment on"),
                            Some(ref github_url) => {
                                send_line(None, &format!("    will comment on {github_url}"))
                            }
                        };
                    }
                    _ => {
                        send_line(None, &format!("  {channel} (no topic data buffered)"));
                    }
                }
            }
        }
        "bye" => {
            if response_target.starts_with('#') {
                let mut this_channel_data = irc_state
                    .channel_data(response_target, config)
                    .write()
                    .unwrap();
                this_channel_data.end_topic(irc);
                irc.send(Command::PART(
                    String::from(response_target),
                    Some(format!(
                        "Leaving at request of {}.  Feel free to /invite me back.",
                        response_username.unwrap()
                    )),
                ))
                .unwrap();
            } else {
                send_line(response_username, "'bye' only works in a channel");
            }
        }
        "end topic" => {
            if response_target.starts_with('#') {
                let mut this_channel_data = irc_state
                    .channel_data(response_target, config)
                    .write()
                    .unwrap();
                this_channel_data.end_topic(irc);
            } else {
                send_line(response_username, "'end topic' only works in a channel");
            }
        }
        "reboot" => {
            let mut channels_with_topics = irc_state
                .channel_data
                .iter()
                .filter_map(|(channel, channel_data)| {
                    if channel_data.read().unwrap().current_topic.is_some() {
                        Some(channel)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            if channels_with_topics.is_empty() {
                send_line(response_username, "OK, I'll reboot now.");

                // quit from the server, with a message
                irc.send(Command::QUIT(Some(format!(
                    "{}, rebooting at request of {}.",
                    code_description(),
                    response_username.unwrap()
                ))))
                .unwrap();

                // Wait for 500ms to allow the sending to complete.
                // FIXME: Should actually wait on something appropriate!
                let timeout = tokio::time::sleep(Duration::from_millis(500)).map(|()| {
                    // exit, and assume whatever started the bot will restart it
                    std::process::exit(0);
                });
                let _ = tokio::spawn(timeout);
            } else {
                // refuse to reboot
                channels_with_topics.sort();
                send_line(
                    response_username,
                    &format!(
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
pub struct IRCState {
    channel_data: HashMap<String, Arc<RwLock<ChannelData>>>,
    github_type: GithubType,
}

impl IRCState {
    /// Create an empty IRCState.
    pub fn new(github_type_: GithubType) -> IRCState {
        IRCState {
            channel_data: HashMap::new(),
            github_type: github_type_,
        }
    }

    fn channel_data(
        &mut self,
        channel: &str,
        config: &'static BotConfig,
    ) -> &Arc<RwLock<ChannelData>> {
        let github_type = self.github_type;
        self.channel_data
            .entry(String::from(channel))
            .or_insert_with(|| {
                Arc::new(RwLock::new(ChannelData::new(channel, config, github_type)))
            })
    }
}

struct ChannelLine {
    source: String,
    is_action: bool,
    message: String,
}

struct TopicData {
    topic: String,
    group: String,
    github_url: Option<String>,
    lines: Vec<ChannelLine>,
    resolutions: Vec<String>,
    remove_from_agenda: bool,
    publish_resolutions_only: bool,
}

struct ChannelData {
    channel_name: String,
    current_topic: Option<TopicData>,
    config: &'static BotConfig,
    github_type: GithubType,
    last_activity: Instant,
    have_activity_timeout: bool,
    activity_timeout_duration: Duration,
}

impl fmt::Display for ChannelLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_action {
            write!(f, "* {} {}", self.source, self.message)
        } else {
            write!(f, "<{}> {}", self.source, self.message)
        }
    }
}

impl TopicData {
    fn new(topic: &str, group: &str, publish_resolutions_only: bool) -> TopicData {
        let topic_ = String::from(topic);
        let group_ = String::from(group);
        let publish_resolutions_only_ = publish_resolutions_only;
        TopicData {
            topic: topic_,
            group: group_,
            github_url: None,
            lines: vec![],
            resolutions: vec![],
            remove_from_agenda: false,
            publish_resolutions_only: publish_resolutions_only_,
        }
    }

    fn should_comment(&self) -> bool {
        self.github_url.is_some()
            && (!self.resolutions.is_empty() || !self.publish_resolutions_only)
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
    format!("{tick_string}{space_first}{s}{space_last}{tick_string}")
}

fn escape_for_html_block(s: &str) -> String {
    // Insert a zero width no-break space (U+FEFF, also byte order mark) between
    // word-starting-# and a digit, so that github doesn't linkify things like "#1"
    // into links to github issues.
    //
    // Do this first, in case we later start doing escaping that produces HTML
    // numeric character references in decimal.
    static ISSUE_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?P<space>[[:space:]])[#](?P<number>[0-9])").unwrap());
    let no_issue_links = ISSUE_RE.replace_all(s, "${space}#\u{feff}${number}");

    no_issue_links.replace('&', "&amp;").replace('<', "&lt;")
}

impl fmt::Display for TopicData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Use `...` around the topic and resolutions, and ```-escaping around
        // the IRC log to avoid most concern about escaping.
        write!(
            f,
            "The {} just discussed {}",
            self.group,
            if self.topic.is_empty() {
                String::from("this issue")
            } else {
                escape_as_code_span(&self.topic)
            }
        )?;
        if self.resolutions.is_empty() {
            writeln!(f, ".")?;
        } else {
            write!(f, ", and agreed to the following:\n\n")?;
            for resolution in &self.resolutions {
                writeln!(f, "* {}", escape_as_code_span(resolution))?;
            }
        }

        if !self.publish_resolutions_only {
            write!(
                f,
                "\n<details><summary>The full IRC log of that \
                 discussion</summary>\n"
            )?;
            for line in &self.lines {
                writeln!(f, "{}<br>", escape_for_html_block(&format!("{line}")))?;
            }
            writeln!(f, "</details>")?;
        }
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
        Some(String::from(s[prefix.len()..].trim_start()))
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
        .filter_map(|prefix| strip_ci_prefix(s, prefix))
        .next()
}

impl ChannelData {
    fn new(
        channel_name_: &str,
        config: &'static BotConfig,
        github_type_: GithubType,
    ) -> ChannelData {
        let activity_timeout_duration_ = Duration::from_secs(60 * config.activity_timeout_minutes);
        let use_activity_timeouts = activity_timeout_duration_ > Duration::from_secs(0);

        ChannelData {
            channel_name: String::from(channel_name_),
            current_topic: None,
            config,
            github_type: github_type_,
            last_activity: Instant::now(),
            // If we're not using activity timeouts, disable them by pretending to already have
            // one.
            have_activity_timeout: !use_activity_timeouts,
            activity_timeout_duration: activity_timeout_duration_,
        }
    }

    // Returns the response that should be sent to the message over IRC.
    // FIXME: Move this to be a method on IRCState.
    fn add_line(&mut self, irc: &'static IrcClient, target: &str, line: ChannelLine) {
        if !line.is_action {
            if let Some(ref topic) = strip_ci_prefix(&line.message, "topic:") {
                self.start_topic(irc, topic);
            } else if let Some(ref subtopic) = strip_ci_prefix(&line.message, "subtopic:") {
                // Treat subtopic: the same as topic:, at least for now.
                self.start_topic(irc, subtopic);
            }
        }
        if (line.is_action
            && line.source == "trackbot"
            && line.message == "is ending a teleconference.")
            || (!line.is_action
                && line.source == "Zakim"
                && line
                    .message
                    .starts_with("As of this point the attendees have been"))
        {
            self.end_topic(irc);
        }
        let respond_with = {
            let target = target.to_owned();
            move |response| {
                send_irc_line(irc, &target, true, response);
            }
        };
        match self.current_topic {
            None => {
                let response =
                    match extract_github_url(&line.message, self.config, target, &None, false) {
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
                    };
                let _ = response.map(respond_with);
            }
            Some(ref mut data) => {
                let (new_url_option, extract_failure_response) =
                    extract_github_url(&line.message, self.config, target, &data.github_url, true);
                match (new_url_option.as_ref(), &data.github_url) {
                    (None, _) => {
                        let _ = extract_failure_response.map(respond_with);
                    }
                    (Some(&None), &None) => (),
                    (Some(&None), _) => {
                        respond_with(String::from("OK, I won't post this discussion to GitHub."));
                    }
                    (Some(new_url), old_url) if *old_url == *new_url => (),
                    (Some(Some(new_url)), old_url_option) => {
                        let respond_title_future = fetch_github_title(self.config, self.github_type, new_url.clone()).map_ok({
                            let old_url_option = old_url_option.clone();
                            let new_url = new_url.clone();
                            move |title| {
                                match old_url_option {
                                    None => respond_with(format!("OK, I'll post this discussion to {new_url} ({title}).")),
                                    Some(old_url) => respond_with(format!("OK, I'll post this discussion to {new_url} ({title}) instead of {old_url} like you said before.")),
                                }
                            }
                        });
                        let _ = tokio::spawn(respond_title_future);
                    }
                };

                if let Some(new_url) = new_url_option {
                    data.github_url = new_url;
                }

                if !line.is_action {
                    let is_resolution = line.message.starts_with("RESOLUTION")
                        || line.message.starts_with("RESOLVED");
                    let is_summary = line.message.starts_with("SUMMARY");
                    let is_action = line.message.starts_with("ACTION");

                    if is_resolution || is_summary || is_action {
                        data.resolutions.push(line.message.clone());
                    }

                    if is_resolution {
                        data.remove_from_agenda = true;
                    }

                    data.lines.push(line);
                };
            }
        }
    }

    // FIXME: Move this to be a method on IRCState.
    fn start_topic(&mut self, irc: &'static IrcClient, topic: &str) {
        self.end_topic(irc);
        let channel_config = &self
            .config
            .channels
            .get(&self.channel_name)
            .expect("How are we in an unconfigured channel?");
        let group = &channel_config.group;
        self.current_topic = Some(TopicData::new(
            topic,
            group,
            channel_config.publish_resolutions_only,
        ));
    }

    // FIXME: Move this to be a method on IRCState.
    fn end_topic(&mut self, irc: &'static IrcClient) {
        // TODO: Test the topic boundary code.
        if let Some(topic) = self.current_topic.take() {
            if topic.should_comment() {
                let task = GithubCommentTask::new(
                    irc,
                    &self.channel_name,
                    topic,
                    self.config,
                    self.github_type,
                );
                let _ = tokio::spawn(task.run());
            }
        }
    }
}

/// Given a string that is the URL of a github issue or PR, return a
/// future with the title.
async fn fetch_github_title<S>(
    config: &'static BotConfig,
    github_type: GithubType,
    s: S,
) -> Result<String, ()>
where
    S: Into<String>,
{
    let new_url = GithubURL::from_string(s).expect("regexp failure");
    let github = github_connection(config, github_type);
    Ok(match github {
        // When mocking the github connection for tests, pretend it's "TITLE".
        // FIXME: Are there now better methods for this in futures 0.3?
        None => String::from("TITLE"),
        Some(github) => github
            .issues()
            .get(&new_url.owner, &new_url.repo, new_url.number)
            .await
            .map_or_else(
                |err| format!("COULDN'T GET TITLE due to error {err:?}"),
                |response| response.body.title,
            ),
    })
}

/// extract_github_url can be run on any regular line of text received
/// over IRC.  It returns a pair where:
///  * the first item is a nested option, the outer option representing
///    whether to replace the current github URL, and the inner option
///    being part of that URL (so that we can replace to no-url)
///  * the second item being a response to send over IRC, if needed, which
///    will only be present if the first item is None
fn extract_github_url(
    message: &str,
    config: &BotConfig,
    target: &str,
    current_github_url: &Option<String>,
    in_topic: bool,
) -> (Option<Option<String>>, Option<String>) {
    static GITHUB_URL_PART_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"https://github.com/(?P<repo>[^/]*/[^/]*)/(issues|pull)/(?P<number>[0-9]+)")
            .unwrap()
    });
    if let Some(ref maybe_url) = strip_one_ci_prefix(
        message,
        ["github:", "github topic:", "github issue:"].iter(),
    ) {
        if maybe_url.to_lowercase() == "none" {
            (Some(None), None)
        } else {
            check_github_url(maybe_url, config, target)
        }
    } else {
        match GITHUB_URL_PART_RE.find(message) {
            Some(ref rematch) => {
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
            }
            _ => (None, None),
        }
    }
}

/// check_github_url is just like extract_github_url except that it only
/// handles a URL argument.  It is used by extract_github_url and by the
/// handling of the "take up" command.
fn check_github_url(
    maybe_url: &str,
    config: &BotConfig,
    target: &str,
) -> (Option<Option<String>>, Option<String>) {
    static GITHUB_URL_WHOLE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?P<issueurl>https://github.com/(?P<owner>[^/]*)/(?P<repo>[^/]*)/(issues|pull)/(?P<number>[0-9]+))([#][^ ]*)?$").unwrap()
    });
    match GITHUB_URL_WHOLE_RE.captures(maybe_url) {
        Some(ref caps) => {
            let channel_config = config.channels.get(target);
            if channel_config.is_none() {
                (
                    None,
                    Some(String::from(
                        "I can't comment on that github issue because I don't have a configuration of allowed repositories for this channel.",
                    )),
                )
            } else {
                let allowed_repos = &channel_config.unwrap().github_repos_allowed;
                let is_allowed = allowed_repos.iter().any(|r| {
                    let pos = match r.find('/') {
                        Some(pos) => pos,
                        None => return false,
                    };
                    let (owner, repo) = r.split_at(pos);
                    let repo = &repo[1..];
                    owner == &caps["owner"] && (repo == &caps["repo"] || repo == "*")
                });
                if is_allowed {
                    (Some(Some(String::from(&caps["issueurl"]))), None)
                } else {
                    (
                        None,
                        Some(format!(
                            "I can't comment on that github issue because it's not in \
                         a repository I'm allowed to comment on, which are: {}.",
                            allowed_repos.join(" "),
                        )),
                    )
                }
            }
        }
        _ => (
            None,
            Some(String::from(
                "I can't comment on that because it doesn't look like a \
                 github issue to me.",
            )),
        ),
    }
}

struct GithubURL {
    url: String, // The whole URL, of which the below are parts.
    owner: String,
    repo: String,
    number: i64,
}

impl GithubURL {
    fn from_string<S>(s: S) -> Option<GithubURL>
    where
        S: Into<String>,
    {
        static GITHUB_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^https://github.com/(?P<owner>[^/]*)/(?P<repo>[^/]*)/(issues|pull)/(?P<number>[0-9]+)$").unwrap()
        });

        let s = s.into();
        let mut result = GITHUB_URL_RE.captures(&s).as_ref().map(|caps| GithubURL {
            url: String::from(""),
            owner: String::from(&caps["owner"]),
            repo: String::from(&caps["repo"]),
            number: caps["number"].parse::<i64>().unwrap(),
        });
        if let Some(ref mut result) = result {
            result.url = s;
        }
        result
    }
}

// Return Some(connection) when we're really connecting and None if we're
// mocking the connection.
fn github_connection(config: &BotConfig, github_type: GithubType) -> Option<GithubClient> {
    match github_type {
        GithubType::RealGithubConnection => Some(
            GithubClient::new(
                config.github_uastring.as_str(),
                Some(GithubCredentials::Token(config.github_access_token.clone())),
            )
            .unwrap(),
        ),
        GithubType::MockGithubConnection => None,
    }
}

struct RemoveLabelTask {
    github: GithubClient,
    owner: String,
    repo: String,
    number: i64,
    label: String,
}

impl RemoveLabelTask {
    async fn run(&self) -> Result<String, ()> {
        let remove_result = self
            .github
            .issues()
            .remove_label(&self.owner, &self.repo, self.number, &self.label)
            .await;
        let label = &self.label;
        Ok(match remove_result {
            Ok(_) => format!(" and removed the \"{label}\" label"),
            Err(err) => format!(" and UNABLE TO REMOVE LABEL \"{label}\" due to error: {err:?}"),
        })
    }
}

struct GithubCommentTask {
    // a clone of the IRCServer is OK, because it reference-counts almost all of its internals
    irc: &'static IrcClient,
    response_target: String,
    data: TopicData,
    github: Option<GithubClient>, /* None means we're mocking the connection */
}

impl GithubCommentTask {
    fn new(
        irc_: &'static IrcClient,
        response_target_: &str,
        data_: TopicData,
        config: &BotConfig,
        github_type_: GithubType,
    ) -> GithubCommentTask {
        let github_ = github_connection(config, github_type_);
        GithubCommentTask {
            irc: irc_,
            response_target: String::from(response_target_),
            data: data_,
            github: github_,
        }
    }

    async fn run(self) {
        if let Some(ref github_url) = self.data.github_url {
            if let Some(github_url) = GithubURL::from_string(github_url.clone()) {
                let comment_text = format!("{}", self.data);

                let send_response = {
                    let irc = self.irc;
                    let target = self.response_target.clone();
                    move |response: String| {
                        send_irc_line(irc, &target, true, response);
                    }
                };
                match self.github {
                    Some(ref github) => {
                        let owner = github_url.owner;
                        let repo = github_url.repo;
                        let num = github_url.number;
                        let url = github_url.url;
                        let issues = github.issues();
                        // Despite documentation, 0 and 0 (which are the values octorust omits)
                        // seems to be the only combination that works here.
                        let labels_result =
                            issues.list_labels_on_issue(&owner, &repo, num, 0, 0).await;
                        let response_text = match labels_result {
                            Err(err) => {
                                format!("UNABLE TO RETRIEVE LABELS ON {url} due to error: {err:?}")
                            }
                            Ok(labels_response) => {
                                // TODO: Add the comment in parallel with retrieving the labels.
                                let comment_body = PullsUpdateReviewRequest { body: comment_text };
                                let comment_task = issues.create_comment(&owner, &repo, num, &comment_body).then({
                                        let url = url.clone();
                                        move |result| {
                                            ok::<String, ()>(match result {
                                                Ok(_) => format!("Successfully commented on {url}"),
                                                Err(err) => format!(
                                                    "UNABLE TO COMMENT on {url} due to error: {err:?}"
                                                ),
                                            })
                                        }
                                    });

                                let mut label_tasks = Vec::new();
                                if self.data.remove_from_agenda {
                                    // We had resolutions, so remove any label starting with
                                    // "Agenda+" (such as "Agenda+", "Agenda+ F2F", "Agenda+
                                    // TPAC", etc.).
                                    for label_obj in labels_response.body {
                                        let label = label_obj.name;
                                        if label.starts_with("Agenda+") {
                                            label_tasks.push(RemoveLabelTask {
                                                github: github.clone(),
                                                owner: owner.clone(),
                                                repo: repo.clone(),
                                                number: num,
                                                label: label.clone(),
                                            });
                                        }
                                    }
                                }

                                let (comment_msg, label_msg_vec) = join!(
                                    comment_task,
                                    futures::future::join_all(label_tasks.iter().map(|t| t.run()))
                                );
                                iter::once(&comment_msg)
                                    .chain(label_msg_vec.iter())
                                    .flat_map(|s| s.as_ref().unwrap().chars())
                                    .collect::<String>()
                            }
                        };
                        send_response(response_text);
                    }
                    None => {
                        // Mock the github comments by sending them over IRC
                        // to a fake user called github-comments.
                        let send_github_comment_line = |line: &str| {
                            send_irc_line(self.irc, "github-comments", false, String::from(line))
                        };
                        send_github_comment_line(
                            format!("!BEGIN GITHUB COMMENT IN {}", github_url.url).as_str(),
                        );
                        for line in comment_text.split('\n') {
                            send_github_comment_line(line);
                        }
                        send_github_comment_line(
                            format!("!END GITHUB COMMENT IN {}", github_url.url).as_str(),
                        );
                        send_response(format!(
                            "{} on {}",
                            "Successfully commented", github_url.url
                        ));
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
            strip_one_ci_prefix("GitHub:url goes here", ["issue:", "github:"].iter()),
            Some(String::from("url goes here"))
        );
        assert_eq!(
            strip_one_ci_prefix("GITHUB: url goes here", ["issue:", "github:"].iter()),
            Some(String::from("url goes here"))
        );
        assert_eq!(
            strip_one_ci_prefix("issue: url goes here", ["issue:", "github:"].iter()),
            Some(String::from("url goes here"))
        );
        assert_eq!(
            strip_one_ci_prefix("topic: url goes here", ["issue:", "github:"].iter()),
            None
        );
    }
}
