// see 'rustc -W help'
#![warn(missing_docs, unused_extern_crates, unused_results)]

//! Test all of the tests in chats/, which are .txt files formatted with IRC
//! input beginning with <, expected IRC output beginning with >, and expected
//! github output beginning with !.

use futures::prelude::*;
use futures::task::Poll;
use irc::client::prelude::{Client as IrcClient, Config as IrcConfig};
use lazy_static::lazy_static;
use log::{debug, info};
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::str;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::time::{Duration, Instant};
use wgmeeting_github_ircbot::*;

const MOCK_SERVER_HOST: &str = "127.0.0.1";
const MOCK_SERVER_PORT: u16 = 43210;

#[tokio::test]
async fn test_chats() -> Result<(), failure::Error> {
    env_logger::init();

    let chats_dir = Path::new(file!()).parent().unwrap().join("chats");
    info!("Going through {:?}", chats_dir);
    let mut fail_count = 0;
    for direntry in chats_dir.read_dir()? {
        if let Ok(direntry) = direntry {
            if !test_one_chat(direntry.path().as_path()).await? {
                fail_count += 1;
            }
        }
    }
    assert!(
        fail_count == 0,
        format!("{} chat test failure(s), see above", fail_count)
    );

    Ok(())
}

async fn test_one_chat(path: &Path) -> Result<bool, failure::Error> {
    info!("Testing {:?}", path);

    // We're given the path to a file (the chat file) that represents a dialog between the bot
    // and other users on the IRC server, and also contains the comments the bot makes on github
    // issues.

    // All of the lines in the chat file, as a vec (lines) of vecs (bytes).
    let chat_file_lines = {
        let mut file_bytes: Vec<u8> = Vec::new();
        let _size = File::open(path)?.read_to_end(&mut file_bytes)?;
        file_bytes
    }
    .split(|byte| *byte == '\n' as u8)
    .map(|arr| arr.to_vec())
    .collect::<Vec<Vec<u8>>>();

    let is_finished = Cell::new(false);

    let server = mock_irc_server(&chat_file_lines, &is_finished);
    let bot = run_irc_bot(&is_finished);

    let (actual_lines, bot_result) = future::join(server, bot).await;
    let _ = bot_result?;
    let actual_lines = actual_lines?;

    let actual_str = str::from_utf8(actual_lines.as_slice())?;
    let expected_lines = chat_lines_to_expected_lines(&chat_file_lines);
    let expected_str = str::from_utf8(expected_lines.as_slice())?;
    let test_pass = actual_str == expected_str;
    println!("\n{:?} {}", path, if test_pass { "PASS" } else { "FAIL" });
    if !test_pass {
        for d in diff::lines(expected_str, actual_str) {
            match d {
                diff::Result::Left(actual) => println!("-{}", actual),
                diff::Result::Both(actual, _) => println!(" {}", actual),
                diff::Result::Right(expected) => println!("+{}", expected),
            }
        }
    }

    Ok(test_pass)
}

/// Run the fake IRC server for the chat test, driving the dialog based on the chat file.
/// Record the entire conversation and return that recording for comparison with the expected
/// result.
async fn mock_irc_server(chat_file_lines: &Vec<Vec<u8>>, is_finished: &Cell<bool>) -> Result<Vec<u8>, failure::Error> {
    let actual_lines = RefCell::new(Vec::<u8>::new());

    struct WaitLinesData {
        expect_lines: i32,
        wait_deadline: Instant,
    };

    impl WaitLinesData {
        pub fn should_wait(&self) -> bool {
            let time_remains = self.wait_deadline > Instant::now();
            let result = self.expect_lines > 0 && time_remains;
            debug!(
                "should_wait: expect_lines={}, time_remains={} ==> {}",
                self.expect_lines, time_remains, result
            );
            if !time_remains {
                info!("wait for {} lines timed out", self.expect_lines);
            }
            result
        }
    };

    const WAIT_DURATION: Duration = Duration::from_millis(100u64);
    const SERVER_SHUTDOWN_DURATION: Duration = Duration::from_millis(10u64);

    let wait_lines_data = RefCell::new(WaitLinesData {
        expect_lines: 3, // length of identify sequence
        wait_deadline: Instant::now() + WAIT_DURATION,
    });

    let irc_server_addr = format!("{}:{}", MOCK_SERVER_HOST, MOCK_SERVER_PORT);
    let mut irc_server_listener = TcpListener::bind(&irc_server_addr).await?;
    let (mut tcp_stream, _socket_addr) = irc_server_listener.accept().await?;
    tcp_stream.set_nodelay(true)?;
    debug!("IRC server got incoming connection: nodelay={} recv_buffer_size={} send_buffer_size={}", tcp_stream.nodelay()?, tcp_stream.recv_buffer_size()?, tcp_stream.send_buffer_size()?);
    let (reader, mut writer) = tcp_stream.split();

    let reader_future = async {
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await? {
            if line.starts_with("PING ") {
                continue
            }
            debug!("IRC server read line: {}", line);

            {
                let mut wait_lines_data = wait_lines_data.borrow_mut();
                wait_lines_data.expect_lines = wait_lines_data.expect_lines - 1;
            }

            {
                let mut actual_lines = actual_lines.borrow_mut();
                actual_lines.append(&mut ">".bytes().collect());
                actual_lines.extend_from_slice(
                    line.chars()
                        .flat_map(|c| c.escape_default())
                        .collect::<String>()
                        .as_bytes(),
                );
                actual_lines.append(&mut "\r\n".bytes().collect());
            }
        }

        Ok::<(), failure::Error>(())
    };

    let writer_future = async {
        for line in chat_file_lines.iter() {
            let first_char = line.first().map(|b| *b as char);
            if first_char == Some('>') || first_char == Some('!') {
                // This is a line we should expect to recieve from the bot.  Note this in
                // |wait_lines_data|, which |reader_future| will use to adjust its timing.
                let mut wait_lines_data = wait_lines_data.borrow_mut();
                wait_lines_data.expect_lines = wait_lines_data.expect_lines + 1;
                wait_lines_data.wait_deadline = Instant::now() + WAIT_DURATION;
            }

            if first_char != Some('<') {
                continue
            }

            while wait_lines_data.borrow().should_wait() {
                tokio::time::delay_for(Duration::from_millis(1)).await;
            }

            // note that line still begins with '<'
            // FIXME: Clean up this total hack for \u{1} !
            // (The other direction uses escape_default().)
            let mut line_str = str::from_utf8(&line[1..])?.replace("\\u{1}", "\u{1}");
            debug!("IRC server writing line: {}", line_str);
            line_str.push_str("\r\n");

            {
                let mut actual_lines = actual_lines.borrow_mut();
                actual_lines.extend_from_slice(&line);
                actual_lines.append(&mut "\r\n".bytes().collect());
            }

            let _ = writer.write_all(line_str.as_bytes()).await?;
        }

        tokio::time::delay_for(SERVER_SHUTDOWN_DURATION).await;

        debug!("SHUTTING DOWN THE SERVER");
        is_finished.set(true);
        // This seems (to my surprise) to be good enough to make the reader terminate as well.
        writer.shutdown().await?;

        Ok::<(), failure::Error>(())
    };

    let (reader_result, writer_result) = future::join(reader_future, writer_future).await;
    reader_result?;
    writer_result?;

    Ok(actual_lines.into_inner())
}

/// Run the IRC bot side of the chat test (i.e., the code we're testing).
async fn run_irc_bot(is_finished: &Cell<bool>) -> Result<(), failure::Error> {
    let irc_config = IrcConfig {
        use_mock_connection: false,
        owners: vec![format!("dbaron")],
        nickname: Some(format!("test-github-bot")),
        alt_nicks: vec![format!("test-github-bot-"), format!("test-github-bot--")],
        username: Some(format!("dbaron-gh-bot")),
        realname: Some(format!("Bot to add meeting minutes to github issues.")),
        server: Some(format!("{}", MOCK_SERVER_HOST)),
        port: Some(MOCK_SERVER_PORT),
        use_tls: Some(false),
        encoding: Some(format!("UTF-8")),
        channels: vec![format!("#meetingbottest"), format!("#testchannel2")],
        user_info: Some(format!("Bot to add meeting minutes to github issues.")),

        // In testing mode, we send the github comments as IRC messages, so we
        // need to be able to handle more substantial bursts of messages
        // without delay.
        burst_window_length: Some(0),
        max_messages_in_burst: Some(50),
        ..Default::default()
    };
    lazy_static! {
        static ref BOT_CONFIG: BotConfig = BotConfig {
            source: "https://github.com/dbaron/wgmeeting-github-ircbot".to_string(),
            channels: vec![
                (format!("#meetingbottest"), ChannelConfig {
                    group: format!("Bot-Testing Working Group"),
                    github_repos_allowed: vec![
                        "dbaron/wgmeeting-github-ircbot".to_string(),
                        "dbaron/nonexistentrepo".to_string(),
                        "upsuper/*".to_string(),
                    ],
                }),
                (format!("#testchannel2"), ChannelConfig {
                    group: format!("Second Bot-Testing Working Group"),
                    github_repos_allowed: vec![
                        "dbaron/wgmeeting-github-ircbot".to_string(),
                    ],
                }),
            ].into_iter().collect(),
            // Use of a 0 value disables timeouts, which is needed to avoid intermittent
            // failures (using really-0 timeouts) or having the event loop wait until the
            // timeout completes (positive timeouts).
            activity_timeout_minutes: 0,
            owners: vec![format!("dbaron")],
            ..Default::default()
        };
    }

    let mut irc_state = IRCState::new(GithubType::MockGithubConnection);

    let irc_client: &'static mut _ = Box::leak(Box::new(
        IrcClient::from_config(irc_config).await?,
    ));

    irc_client.identify()?;

    let finished_cb = {
        future::poll_fn(move |_cx| {
            if is_finished.get() {
                debug!("in take_until callback for messages stream: terminating");
                Poll::Ready(())
            } else {
                debug!("in take_until callback for messages stream: continuing");
                Poll::Pending
            }
        })
    };

    let mut irc_stream = irc_client.stream()?.take_until(finished_cb);
    while let Some(message) = irc_stream.next().await.transpose()? {
        process_irc_message(irc_client, &mut irc_state, &BOT_CONFIG, message);
    }

    Ok(())
}

/// Convert the lines in the chat file to the dialog that the test should expect to have been
/// recorded by the IRC server.
fn chat_lines_to_expected_lines(chat_file_lines: &Vec<Vec<u8>>) -> Vec<u8> {
    let mut expected_lines = ">CAP END\r\n>NICK test-github-bot\r\n>USER dbaron-gh-bot 0 * :Bot to add meeting minutes to github issues.\r\n".bytes().collect::<Vec<u8>>();

    for line in chat_file_lines.iter() {
        match line.first().map(|b| *b as char) {
            // just skip blank lines (and the empty string after the last
            // line, given the use of split)
            None => (),
            Some('<') => {
                let line = str::from_utf8(line).unwrap();
                expected_lines.extend_from_slice(line.as_bytes());
                expected_lines.append(&mut "\r\n".bytes().collect());
            }
            Some('>') => {
                expected_lines.extend(
                    str::from_utf8(line)
                        .unwrap()
                        .replace("[[CODE_DESCRIPTION]]", &*code_description())
                        .bytes(),
                );
                expected_lines.append(&mut "\r\n".bytes().collect());
            }
            Some('!') => {
                // for now, we send the github comments over IRC when
                // testing, but we don't encode that into the chat
                // format
                expected_lines
                    .append(&mut ">PRIVMSG github-comments ".bytes().collect());
                // Match use of ":" in the stringify function in irc-proto's
                // src/command.rs.
                if line.len() == 1 || line.contains(&0x20u8 /* space */) {
                    expected_lines.append(&mut ":".bytes().collect());
                }
                expected_lines.extend_from_slice(&line[1..]);
                expected_lines.append(&mut "\r\n".bytes().collect());
            }
            _ => {
                panic!(
                    //"Unexpected line in test file {:?}:\n{}",
                    // path,
                    "Unexpected line in test file:\n{}",
                    str::from_utf8(line).unwrap_or("[non-UTF-8 line]")
                );
            }
        }
    }

    expected_lines
}
