// see 'rustc -W help'
#![warn(missing_docs, unused_extern_crates, unused_results)]

//! Test all of the tests in chats/, which are .txt files formatted with IRC
//! input beginning with <, expected IRC output beginning with >, and expected
//! github output beginning with !.

extern crate diff;
extern crate env_logger;
extern crate futures;
extern crate irc;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate log;
extern crate tokio_core;
extern crate tokio_io;
extern crate wgmeeting_github_ircbot;

mod take_while_external_condition;

use futures::prelude::*;
use irc::client::prelude::{Client, ClientExt, Config, Future, IrcClient, Stream};
use irc::client::PackedIrcClient;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::File;
use std::io::Read;
use std::iter::FromIterator;
use std::path::Path;
use std::time::{Duration, Instant};
use std::str;
use std::rc::Rc;
use tokio_core::reactor::Core;
use tokio_core::reactor::Timeout;
use tokio_core::net::TcpListener;
use tokio_io::AsyncRead;
use tokio_io::codec::LinesCodec;
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

fn panic_on_err<T>(err: T) -> ()
where
    T: Debug,
{
    panic!("{:?}", err);
}

fn to_irc_error<T>(err: T) -> irc::error::IrcError
where
    irc::error::IrcError: From<T>,
{
    irc::error::IrcError::from(err)
}

fn test_one_chat(path: &Path) -> bool {
    info!("Testing {:?}", path);

    let mut core = Core::new().unwrap();
    let handle = core.handle();

    // All of the lines in the file, as a vec (lines, backwards) of vecs (bytes,
    // forwards).
    let mut chat_file_reversed_lines = {
        let mut file_bytes: Vec<u8> = Vec::new();
        let _size = File::open(path)
            .unwrap()
            .read_to_end(&mut file_bytes)
            .unwrap();
        file_bytes
    }.split(|byte| *byte == '\n' as u8)
        .map(|arr| arr.to_vec())
        .rev()
        .collect::<Vec<Vec<u8>>>();

    let actual_lines_cell = Rc::new(RefCell::new(Vec::<u8>::new()));
    let expected_lines_cell = Rc::new(RefCell::new(
        ">CAP END\r\n>NICK :test-github-bot\r\n>USER dbaron-gh-bot 0 * :Bot \
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

    let wait_lines_data_cell = Rc::new(RefCell::new(WaitLinesData {
        expect_lines: 3, // length of identify sequence
        wait_deadline: Instant::now() + Duration::from_millis(10),
    }));

    // A stream of the file data, but which will be slowed down so that it is
    // Async::NotReady when we ought to be waiting for input from the IRC server
    // instead.
    let file_data_stream = futures::stream::poll_fn({
        let wait_lines_data_cell = wait_lines_data_cell.clone();
        let expected_lines_cell = expected_lines_cell.clone();
        let handle = handle.clone();
        move || -> Poll<Option<Vec<u8>>, std::io::Error> {
            debug!("in poll_fn");
            if wait_lines_data_cell.borrow().should_wait() {
                // FIXME: Is this timer really needed?
                debug!("starting should_wait timer");
                handle.spawn(
                    Timeout::new(Duration::from_millis(1), &handle)
                        .unwrap()
                        .map_err(panic_on_err)
                        .and_then(|()| {
                            debug!("should_wait timer finished");
                            Ok(())
                        }),
                );

                Ok(Async::NotReady)
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
                                wait_lines_data.wait_deadline =
                                    Instant::now() + Duration::from_millis(10);
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
                                wait_lines_data.wait_deadline =
                                    Instant::now() + Duration::from_millis(10);
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
                                .append(&mut ">PRIVMSG github-comments :".bytes().collect());
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
                Ok(Async::Ready(line_option))
            }
        }
    });

    // Note that this leaves the initial '<' in the line.
    let lines_to_write_stream = file_data_stream.filter(|line| line.first() == Some(&('<' as u8)));

    lazy_static! {
        static ref IRC_CONFIG: Config = Config {
            use_mock_connection: Some(false),
            owners: Some(vec![format!("dbaron")]),
            nickname: Some(format!("test-github-bot")),
            alt_nicks: Some(vec![
                format!("test-github-bot-"),
                format!("test-github-bot--"),
            ]),
            username: Some(format!("dbaron-gh-bot")),
            realname: Some(format!("Bot to add meeting minutes to github issues.")),
            server: Some(format!("127.0.0.1")),
            port: Some(43210),
            use_ssl: Some(false),
            encoding: Some(format!("UTF-8")),
            channels: Some(vec![format!("#meetingbottest")]),
            user_info: Some(format!("Bot to add meeting minutes to github issues.")),

            // In testing mode, we send the github comments as IRC messages, so we
            // need to be able to handle more substantial bursts of messages
            // without delay.
            burst_window_length: Some(0),
            max_messages_in_burst: Some(50),

            // FIXME: why doesn't this work as documented?
            // source: Some(format!("https://github.
            // com/dbaron/wgmeeting-github-ircbot")),
            options: Some(HashMap::from_iter(vec![
                (
                    format!("source"),
                    format!("https://github.com/dbaron/wgmeeting-github-ircbot"),
                ),
                (
                    format!("github_repos_allowed"),
                    format!("dbaron/wgmeeting-github-ircbot dbaron/nonexistentrepo"),
                ),
                (
                    format!("activity_timeout_minutes"),
                    format!("0"),
                ),
            ])),
            ..Default::default()
        };
        static ref OPTIONS : HashMap<String, String> = IRC_CONFIG.options.as_ref().expect("No options property within configuration?").clone();
    }

    let mut irc_state_ = IRCState::new(GithubType::MockGithubConnection, &handle);
    let irc_state = &mut irc_state_;

    let irc_server_addr = (*format!(
        "{}:{}",
        IRC_CONFIG.server.as_ref().unwrap(),
        IRC_CONFIG.port.as_ref().unwrap()
    )).parse()
        .unwrap();
    let irc_server_tcp = TcpListener::bind(&irc_server_addr, &handle).unwrap();
    let irc_server_stream = irc_server_tcp
        .incoming()
        .take(1)
        .into_future()
        .map_err(|(err, _stream)| err)
        .map(|(conn_option, _stream)| conn_option.unwrap())
        .and_then({
            let handle = handle.clone();
            let actual_lines_cell = actual_lines_cell.clone();
            let is_finished_cell = is_finished_cell.clone();
            move |(tcp_stream, _socket_addr)| {
                tcp_stream.set_nodelay(true).unwrap();
                debug!("IRC server got incoming connection: nodelay={} recv_buffer_size={} send_buffer_size={}", tcp_stream.nodelay().unwrap(), tcp_stream.recv_buffer_size().unwrap(), tcp_stream.send_buffer_size().unwrap());
                let (writer, reader) = tcp_stream.framed(LinesCodec::new()).split();
                let writer_cell = Rc::new(RefCell::new(writer));
                let logged_lines_future = take_while_external_condition::new(reader, {
                    let is_finished_cell = is_finished_cell.clone();
                    move || {
                        debug!(
                            "in take_while callback for logged_lines: {}",
                            if is_finished_cell.get() {
                                "terminating"
                            } else {
                                ""
                            }
                        );
                        !is_finished_cell.get()
                    }
                }).for_each({
                    let actual_lines_cell = actual_lines_cell.clone();
                    move |line| {
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
                        Ok(())
                    }
                });
                let send_lines_future = lines_to_write_stream
                    .and_then({
                        let actual_lines_cell = actual_lines_cell.clone();
                        let writer_cell = writer_cell.clone();
                        move |line| {
                            {
                                let mut actual_lines = actual_lines_cell.borrow_mut();
                                let mut writer = writer_cell.borrow_mut();

                                // note that line still begins with '<'
                                // FIXME: Clean up this total hack for \u{1} !
                                // (The other direction uses escape_default().)
                                let line_str = str::from_utf8(&line[1..])
                                    .unwrap()
                                    .replace("\\u{1}", "\u{1}");
                                debug!("IRC server writing line: {}", line_str);

                                actual_lines.extend_from_slice(&line);
                                actual_lines.append(&mut "\r\n".bytes().collect());
                                if writer.start_send(String::from(line_str)).unwrap()
                                    != futures::AsyncSink::Ready
                                {
                                    panic!("Sink full");
                                }
                            }
                            futures::future::poll_fn({
                                let writer_cell = writer_cell.clone();
                                move || {
                                    let mut writer = writer_cell.borrow_mut();
                                    writer.poll_complete()
                                }
                            })
                        }
                    })
                    .for_each(|()| Ok(()))
                    .and_then(move |()| Timeout::new(Duration::from_millis(10), &handle))
                    .and_then(move |_timeout| {
                        debug!("SHUTTING DOWN THE SERVER");
                        is_finished_cell.set(true);
                        // tcp_stream.shutdown(std::net::Shutdown::Both)
                        // FIXME: This isn't enough.
                        let mut writer = writer_cell.borrow_mut();
                        writer.close()
                    });
                logged_lines_future.join(send_lines_future)
            }
        })
        .map_err(to_irc_error);

    let irc_client_future = IrcClient::new_future(handle.clone(), &IRC_CONFIG).expect(
        "Couldn't initialize server \
         with given configuration file",
    );

    let ircstream =
        irc_client_future.and_then(|PackedIrcClient(irc_client, irc_outgoing_future)| {
            debug!("have PackedIrcClient, sending identify");
            irc_client.identify().unwrap();
            debug!("sent identify");
            take_while_external_condition::new(irc_client.stream(), {
                let is_finished_cell = is_finished_cell.clone();
                move || {
                    debug!(
                        "in take_while callback for messages stream: {}",
                        if is_finished_cell.get() {
                            "terminating"
                        } else {
                            ""
                        }
                    );
                    !is_finished_cell.get()
                }
            }).for_each(move |message| {
                debug!("got IRC message");
                process_irc_message(&irc_client, irc_state, &OPTIONS, message);
                Ok(())
            })
                .join(irc_outgoing_future)
        });

    debug!("starting core.run");
    let _result = core.run(irc_server_stream.join(ircstream));
    debug!("done core.run");

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
