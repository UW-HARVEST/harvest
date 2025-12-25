use super::*;
use crate::test_util::MockTool;
use std::{fs::read_to_string, sync::mpsc::channel, thread::spawn};

/// Verifies that tracing messages are written into the correct files.
#[test]
fn messages() {
    let mut config = Config::mock();
    let tempdir = tempdir().unwrap();
    config.diagnostics_dir = Some(tempdir.path().to_path_buf());
    info!("AAAA"); // Should not be collected.
    let collector = Collector::initialize(&config).unwrap();
    info!("BBBB");
    // Spawns a new thread with a tool reporter. Returns the join handle and a closure that makes
    // the thread write an info! log message. The thread will exit when the closure is dropped.
    let spawn_tool_thread = |name| {
        let ((send_msg, recv_msg), (send_done, recv_done)) = (channel(), channel());
        let reporter = collector.reporter();
        let join = spawn(move || {
            let (_, tool_reporter) = reporter
                .start_tool_run(&MockTool::new().name(name))
                .unwrap();
            let _guard = tool_reporter.setup_thread_logger();
            while let Ok(msg) = recv_msg.recv() {
                info!("{msg}");
                send_done.send(()).unwrap();
            }
        });
        (join, move |msg: &'static str| {
            send_msg.send(msg).unwrap();
            recv_done.recv().unwrap();
        })
    };
    let (join1, info1) = spawn_tool_thread("tool_a");
    let (join2, info2) = spawn_tool_thread("tool_b");
    let (join3, info3) = spawn_tool_thread("tool_a");
    info!("CCCC");
    info1("DDDD");
    info3("EEEE");
    info!("FFFF");
    info3("GGGG");
    info2("HHHH");
    info3("IIII");
    info1("JJJJ");
    info!("KKKK");
    drop((info1, info2, info3));
    let _ = [join1, join2, join3].map(|j| j.join().unwrap());
    info!("LLLL");
    drop(collector);
    info!("MMMM"); // Should not be collected

    // Verifies the given file (path relative to the diagnostic directory) contains log lines with
    // the given contents in order.
    let verify = |path: &str, expected: &[_]| {
        let contents = read_to_string(PathBuf::from_iter([tempdir.path(), path.as_ref()])).unwrap();
        let mut lines = contents.lines();
        for (i, expected) in expected.iter().enumerate() {
            let line = lines.next().unwrap();
            assert!(
                line.contains(expected),
                "{path} line number {} contains {line}, expected {expected}",
                i + 1
            );
        }
        assert_eq!(lines.next(), None);
    };
    #[rustfmt::skip]
    verify(
        "messages",
        &["BBBB", "CCCC", "DDDD", "EEEE", "FFFF", "GGGG", "HHHH", "IIII", "JJJJ", "KKKK", "LLLL"],
    );
    verify("steps/tool_a_001/messages", &["DDDD", "JJJJ"]);
    verify("steps/tool_b_001/messages", &["HHHH"]);
    verify("steps/tool_a_002/messages", &["EEEE", "GGGG", "IIII"]);
}
