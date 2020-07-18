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
use pin_utils::pin_mut;
use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::rc::Rc;
use std::str;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::time::{Duration, Instant};
use wgmeeting_github_ircbot::*;

#[test]
fn test_chats() {
    env_logger::init();

    let chats_dir = Path::new(file!()).parent().unwrap().join("chats");
    info!("Going through {:?}", chats_dir);
    let mut fail_count = 0;
    for direntry in chats_dir.read_dir().unwrap() {
        if let Ok(direntry) = direntry {
            if !test_one_chat(direntry.path().as_path()) {
                fail_count += 1;
            }
        }
    }
    assert!(
        fail_count == 0,
        format!("{} chat test failure(s), see above", fail_count)
    );
}

fn test_one_chat(path: &Path) -> bool {
    info!("Testing {:?}", path);

    let mut rt = Runtime::new().unwrap();
    let handle = rt.handle();

    // All of the lines in the file, as a vec (lines, backwards) of vecs (bytes,
    // forwards).
    let mut chat_file_reversed_lines = {
        let mut file_bytes: Vec<u8> = Vec::new();
        let _size = File::open(path)
            .unwrap()
            .read_to_end(&mut file_bytes)
            .unwrap();
        file_bytes
    }
    .split(|byte| *byte == '\n' as u8)
    .map(|arr| arr.to_vec())
    .rev()
    .collect::<Vec<Vec<u8>>>();

    let actual_lines_cell = Rc::new(RefCell::new(Vec::<u8>::new()));
    let expected_lines_cell = Rc::new(RefCell::new(
        ">CAP END\r\n>NICK test-github-bot\r\n>USER dbaron-gh-bot 0 * :Bot \
         to add meeting minutes to github issues.\r\n"
            .bytes()
            .collect::<Vec<u8>>(),
    ));

    let is_finished_cell = Rc::new(Cell::new(false));

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

    let wait_lines_data_cell = Rc::new(RefCell::new(WaitLinesData {
        expect_lines: 3, // length of identify sequence
        wait_deadline: Instant::now() + WAIT_DURATION,
    }));

    // A stream of the file data, but which will be slowed down so that it is
    // Poll::Pending when we ought to be waiting for input from the IRC server
    // instead.
    let file_data_stream = stream::poll_fn({
        let wait_lines_data_cell = wait_lines_data_cell.clone();
        let expected_lines_cell = expected_lines_cell.clone();
        let handle = handle.clone();
        move |_cx| -> Poll<Option<Vec<u8>>> {
            debug!("in poll_fn");
            if wait_lines_data_cell.borrow().should_wait() {
                // FIXME: Is this timer really needed?
                debug!("starting should_wait timer");
                let _ = handle.spawn(tokio::time::delay_for(Duration::from_millis(1)).then(|()| {
                    debug!("should_wait timer finished");
                    future::ok::<(), ()>(())
                }));

                Poll::Pending
            } else {
                let line_option = chat_file_reversed_lines.pop();
                if let Some(ref line) = line_option {
                    let mut expected_lines = expected_lines_cell.borrow_mut();
                    debug!(
                        "returning line -{} from file_data_stream's poll_fn: {}",
                        chat_file_reversed_lines.len(),
                        str::from_utf8(line).unwrap()
                    );
                    match line.first().map(|b| *b as char) {
                        // just skip blank lines (and the empty string after the last
                        // line, given the use of split)
                        None => (),
                        Some('<') => {
                            debug!(
                                "adding line to read buffer: {}",
                                str::from_utf8(line).unwrap()
                            );
                            let line = str::from_utf8(line).unwrap();
                            expected_lines.extend_from_slice(line.as_bytes());
                            expected_lines.append(&mut "\r\n".bytes().collect());
                        }
                        Some('>') => {
                            {
                                let mut wait_lines_data = wait_lines_data_cell.borrow_mut();
                                wait_lines_data.expect_lines = wait_lines_data.expect_lines + 1;
                                wait_lines_data.wait_deadline = Instant::now() + WAIT_DURATION;
                            }
                            debug!(
                                "adding line to expected results: {}",
                                str::from_utf8(line).unwrap()
                            );
                            expected_lines.extend(
                                str::from_utf8(line)
                                    .unwrap()
                                    .replace("[[CODE_DESCRIPTION]]", &*code_description())
                                    .bytes(),
                            );
                            expected_lines.append(&mut "\r\n".bytes().collect());
                        }
                        Some('!') => {
                            {
                                let mut wait_lines_data = wait_lines_data_cell.borrow_mut();
                                wait_lines_data.expect_lines = wait_lines_data.expect_lines + 1;
                                wait_lines_data.wait_deadline = Instant::now() + WAIT_DURATION;
                            }
                            // FIXME: Need to get these in the actual data, too!
                            info!(
                                "adding line to expected results: {}",
                                str::from_utf8(line).unwrap()
                            );
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
                Poll::Ready(line_option)
            }
        }
    });

    // Note that this leaves the initial '<' in the line.
    let lines_to_write_stream =
        file_data_stream.filter(|line| future::ready(line.first() == Some(&('<' as u8))));

    let irc_config = IrcConfig {
        use_mock_connection: false,
        owners: vec![format!("dbaron")],
        nickname: Some(format!("test-github-bot")),
        alt_nicks: vec![format!("test-github-bot-"), format!("test-github-bot--")],
        username: Some(format!("dbaron-gh-bot")),
        realname: Some(format!("Bot to add meeting minutes to github issues.")),
        server: Some(format!("127.0.0.1")),
        port: Some(43210),
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

    let mut irc_state_ = IRCState::new(GithubType::MockGithubConnection);
    let irc_state = &mut irc_state_;

    let irc_server_addr: String = (*format!(
        "{}:{}",
        irc_config.server.as_ref().unwrap(),
        irc_config.port.as_ref().unwrap()
    ))
    .parse()
    .unwrap();
    let mut irc_server_tcp = rt.block_on(TcpListener::bind(&irc_server_addr)).unwrap();
    let irc_server_stream = irc_server_tcp
        .accept()
        .then({
            let actual_lines_cell = actual_lines_cell.clone();
            let is_finished_cell = is_finished_cell.clone();
            move |res| {
                let (tcp_stream, _socket_addr) = res.unwrap();
                tcp_stream.set_nodelay(true).unwrap();
                debug!("IRC server got incoming connection: nodelay={} recv_buffer_size={} send_buffer_size={}", tcp_stream.nodelay().unwrap(), tcp_stream.recv_buffer_size().unwrap(), tcp_stream.send_buffer_size().unwrap());
                let (reader, writer) = tcp_stream.into_split();
                let reader = BufReader::new(reader).lines();
                let writer_cell = Rc::new(RefCell::new(writer));
                let logged_lines_future = reader.take_until({
                    let is_finished_cell = is_finished_cell.clone();
                    future::poll_fn(move |_cx| {
                        if is_finished_cell.get() {
                            debug!("in take_until callback for messages stream: terminating");
                            Poll::Ready(())
                        } else {
                            debug!("in take_until callback for messages stream: continuing");
                            Poll::Pending
                        }
                    })
                }).for_each({
                    let actual_lines_cell = actual_lines_cell.clone();
                    move |line_result| {
                        let line = line_result.unwrap();
                        if !line.starts_with("PING ") {
                            debug!("IRC server read line: {}", line);
                            {
                                let mut wait_lines_data = wait_lines_data_cell.borrow_mut();
                                wait_lines_data.expect_lines = wait_lines_data.expect_lines - 1;
                            }
                            {
                                let mut actual_lines = actual_lines_cell.borrow_mut();

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
                        future::ready(())
                    }
                });
                let send_lines_future = lines_to_write_stream
                    .then({
                        let actual_lines_cell = actual_lines_cell.clone();
                        let writer_cell = writer_cell.clone();
                        move |line| {
                            {
                                let mut actual_lines = actual_lines_cell.borrow_mut();

                                // note that line still begins with '<'
                                // FIXME: Clean up this total hack for \u{1} !
                                // (The other direction uses escape_default().)
                                let mut line_str = str::from_utf8(&line[1..])
                                    .unwrap()
                                    .replace("\\u{1}", "\u{1}");
                                debug!("IRC server writing line: {}", line_str);
                                line_str.push_str("\r\n");

                                actual_lines.extend_from_slice(&line);
                                actual_lines.append(&mut "\r\n".bytes().collect());

                                // FIXME: Find a better way to do this!
                                future::poll_fn({
                                    let writer_cell = writer_cell.clone();
                                    move |cx| {
                                        let mut writer = writer_cell.borrow_mut();
                                        let write_future = writer.write(line_str.as_bytes());
                                        pin_mut!(write_future);
                                        write_future.poll(cx)
                                    }
                                })
                            }
                        }
                    })
                    .for_each(|_item| future::ready(()))
                    .then(move |()| tokio::time::delay_for(SERVER_SHUTDOWN_DURATION))
                    .then(move |_timeout| {
                        debug!("SHUTTING DOWN THE SERVER");
                        is_finished_cell.set(true);
                        // tcp_stream.shutdown(std::net::Shutdown::Both)
                        // FIXME: This isn't enough.

                        // FIXME: Find a better way to do this!
                        future::poll_fn(move |cx| {
                            let mut writer = writer_cell.borrow_mut();
                            let shutdown_future = writer.shutdown();
                            pin_mut!(shutdown_future);
                            shutdown_future.poll(cx)
                        })
                    });
                future::join(logged_lines_future, send_lines_future)
            }
        });

    let irc_client: &'static mut _ = Box::leak(Box::new(
        rt.block_on(IrcClient::from_config(irc_config)).unwrap(),
    ));

    irc_client.identify().unwrap();

    let irc_stream = irc_client.stream().unwrap();

    // Work around https://github.com/rust-lang/rust/issues/42574 ?
    let irc_client_: &'static _ = irc_client;

    let irc_future = irc_stream
        .take_until({
            let is_finished_cell = is_finished_cell.clone();
            future::poll_fn(move |_cx| {
                if is_finished_cell.get() {
                    debug!("in take_until callback for messages stream: terminating");
                    Poll::Ready(())
                } else {
                    debug!("in take_until callback for messages stream: continuing");
                    Poll::Pending
                }
            })
        })
        .for_each(move |message_result| {
            debug!("got IRC message");
            process_irc_message(
                &irc_client_,
                irc_state,
                &BOT_CONFIG,
                message_result.unwrap(),
            );
            future::ready(())
        });

    debug!("starting rt.run");
    let _ = rt.block_on(future::join(irc_server_stream, irc_future));
    debug!("done rt.run");

    let actual_lines = actual_lines_cell.borrow();
    let actual_str = str::from_utf8(actual_lines.as_slice()).unwrap();
    let expected_lines = expected_lines_cell.borrow();
    let expected_str = str::from_utf8(expected_lines.as_slice()).unwrap();
    let pass = actual_str == expected_str;
    println!("\n{:?} {}", path, if pass { "PASS" } else { "FAIL" });
    if !pass {
        for d in diff::lines(expected_str, actual_str) {
            match d {
                diff::Result::Left(actual) => println!("-{}", actual),
                diff::Result::Both(actual, _) => println!(" {}", actual),
                diff::Result::Right(expected) => println!("+{}", expected),
            }
        }
    }

    pass
}
