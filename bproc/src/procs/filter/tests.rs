use super::*;
use crate::process::{ConnectionDetail, ProcessInfo, ThrottleLimit};
use crate::procs::model::{ProcessEntry, ProcessKey, ProcessRecord, ProcessSample};
use std::sync::Arc;

fn row(pid: i32, name: &str, cmdline: &[&str]) -> Row {
    Row {
        entry: ProcessEntry {
            record: Arc::new(ProcessRecord {
                key: ProcessKey::new(pid, 1_000),
                ppid: Some(1),
                name: name.into(),
                cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
                uid: Some(501),
                user: Some("alice".into()),
                ..Default::default()
            }),
            sample: ProcessSample::default(),
        },
        net: None,
        exited_at_ms: None,
    }
}

fn with_record(mut r: Row, f: impl FnOnce(&mut ProcessRecord)) -> Row {
    f(Arc::make_mut(&mut r.entry.record));
    r
}

fn with_sample(mut r: Row, f: impl FnOnce(&mut ProcessSample)) -> Row {
    f(&mut r.entry.sample);
    r
}

fn with_net(mut r: Row, f: impl FnOnce(&mut ProcessInfo)) -> Row {
    let mut info = ProcessInfo::new(r.key().pid, r.name().to_string());
    f(&mut info);
    r.net = Some(Arc::new(info));
    r
}

fn node() -> Row {
    row(
        42,
        "node",
        &["/usr/local/bin/node", "server.js", "--port", "8080"],
    )
}

fn ctx() -> ColumnCtx {
    ColumnCtx {
        now_ms: 10_000_000,
        ..Default::default()
    }
}

fn m(query: &str, r: &Row) -> Option<Vec<(usize, usize)>> {
    Filter::parse(query)
        .unwrap_or_else(|e| panic!("{query}: {e:?}"))
        .matches(r, &ctx())
        .map(|m| m.highlights)
}

fn hit(query: &str, r: &Row) -> bool {
    m(query, r).is_some()
}

fn error(query: &str) -> FilterError {
    Filter::parse(query).expect_err(query)
}

#[test]
fn empty_and_whitespace_queries_are_empty() {
    for q in ["", "   ", "\t"] {
        let f = Filter::parse(q).unwrap();
        assert!(f.is_empty());
        assert_eq!(f.source(), q);
        assert!(f.matches(&node(), &ctx()).is_some());
    }
    assert!(!Filter::parse("node").unwrap().is_empty());
}

#[test]
fn plain_words_substring_highlight_command_chars() {
    let r = node();
    assert_eq!(m("server", &r), Some(vec![(20, 26)]));
    // Every occurrence is highlighted.
    let r2 = row(1, "a", &["ab", "ab"]);
    assert_eq!(m("ab", &r2), Some(vec![(0, 2), (3, 5)]));
    assert!(!hit("python", &r));
}

#[test]
fn smart_case() {
    let r = node();
    assert!(hit("SERVER", &row(1, "x", &["SERVER"])));
    assert!(hit("server", &row(1, "x", &["SERVER"])));
    assert!(!hit("Server", &r));
    assert!(hit("Node", &row(1, "Node", &[])));
    assert!(!hit("NODE", &r));
}

#[test]
fn fuzzy_matches_name_and_maps_into_command() {
    let r = node();
    // "nde" is only a subsequence of "node", which starts at char 15.
    assert_eq!(m("nde", &r), Some(vec![(15, 16), (17, 19)]));
    assert!(!hit("edon", &r));
}

#[test]
fn fuzzy_picks_tight_window() {
    let n = Needle::new("ab");
    let mut pos = Vec::new();
    assert!(n.fuzzy("a_ab", |p| pos.push(p)));
    assert_eq!(pos, vec![2, 3]);
    let mut pos = Vec::new();
    assert!(n.fuzzy("xaxxb", |p| pos.push(p)));
    assert_eq!(pos, vec![1, 4]);
    assert!(!n.fuzzy("ba", |_| {}));
    assert!(!n.fuzzy("", |_| {}));
}

#[test]
fn fuzzy_without_name_in_command_falls_back_to_command() {
    let r = row(7, "kworker", &["completely", "different"]);
    assert_eq!(m("kwr", &r), Some(vec![]));
    let r = row(7, "Web Content", &["/Apps/Firefox/Web Content", "-x"]);
    assert_eq!(m("wbc", &r), Some(vec![(14, 15), (16, 17), (18, 19)]));
}

#[test]
fn highlights_are_char_indices_for_unicode() {
    let r = row(1, "café", &["/opt/café", "naïve"]);
    assert_eq!(m("naïve", &r), Some(vec![(10, 15)]));
    assert_eq!(m("caf", &r), Some(vec![(5, 8)]));
    assert_eq!(m("CAFÉ", &row(1, "CAFÉ", &[])), Some(vec![(0, 4)]));
    assert_eq!(m("é", &row(1, "É", &[])), Some(vec![(0, 1)]));
}

#[test]
fn empty_cmdline_uses_name() {
    let r = row(1, "launchd", &[]);
    assert_eq!(m("lnch", &r), Some(vec![(0, 1), (3, 6)]));
    assert_eq!(m("name:aunc", &r), Some(vec![(1, 5)]));
}

#[test]
fn phrases_and_quoted_values() {
    let r = node();
    assert_eq!(m("\"--port 8080\"", &r), Some(vec![(30, 41)]));
    assert!(!hit("\"port  8080\"", &r));
    assert!(hit("cmd:\"server.js --port\"", &r));
    assert!(hit("cmd:\"--port\"", &r));
    // Phrases are not fuzzy.
    assert!(!hit("\"nde\"", &r));
    let quoted = row(1, "x", &["say", "\"hi\""]);
    assert!(hit(r#""say \"hi\"""#, &quoted));
    assert!(hit("-\"python app\"", &r));
}

#[test]
fn double_dash_words_are_plain() {
    let r = node();
    assert!(hit("--port", &r));
    assert!(hit("--port=8080", &row(1, "x", &["x", "--port=8080"])));
    assert!(!hit("--verbose", &r));
}

#[test]
fn negation() {
    let r = node();
    assert!(!hit("-node", &r));
    assert!(hit("-python", &r));
    assert!(hit("-user:root", &r));
    assert!(!hit("-user:alice", &r));
    // Negated terms contribute no highlights.
    assert_eq!(m("server -python", &r), Some(vec![(20, 26)]));
}

#[test]
fn or_binds_tighter_than_and() {
    let r = node();
    assert!(hit("python | node", &r));
    assert!(hit("python|node", &r));
    assert!(hit("server python | node", &r));
    assert!(!hit("ruby python | node", &r));
    assert!(!hit("python | ruby", &r));
    assert!(hit("python | ruby | server", &r));
    assert!(hit("-node | server", &r));
    // All matching alternatives are highlighted.
    assert_eq!(m("server | 8080", &r), Some(vec![(20, 26), (37, 41)]));
}

#[test]
fn text_fields() {
    let r = with_record(node(), |rec| {
        rec.exe = Some("/usr/local/bin/node".into());
        rec.group = Some("docker".into());
        rec.tty = Some("pts/3".into());
    });
    for q in [
        "user:ali",
        "user=alice",
        "user:501",
        "owner:alice",
        "name:node",
        "name=node",
        "cmd:--port",
        "command:server",
        "exe:/usr",
        "group:dock",
        "tty:pts",
        "user!=root",
        "name!=python",
    ] {
        assert!(hit(q, &r), "{q}");
    }
    for q in [
        "user:root",
        "user=ali",
        "name=nod",
        "exe:/opt",
        "group:k8s",
        "tty:tty",
        "user!=alice",
        "user!=501",
    ] {
        assert!(!hit(q, &r), "{q}");
    }
    // Missing optional text only satisfies `!=`.
    assert!(!hit("exe:x", &node()));
    assert!(hit("exe!=x", &node()));
}

#[test]
fn field_highlights() {
    let r = node();
    assert_eq!(m("cmd:port", &r), Some(vec![(32, 36)]));
    assert_eq!(m("name:od", &r), Some(vec![(16, 18)]));
    assert_eq!(m("name=node", &r), Some(vec![(15, 19)]));
    assert_eq!(m("user:alice", &r), Some(vec![]));
    let short = row(1, "sh", &["sh"]);
    assert_eq!(m("cmd=sh", &short), Some(vec![(0, 2)]));
    // Name not present in the command: name: falls back to a command search.
    let renamed = row(1, "worker", &["python", "worker.py"]);
    assert_eq!(m("name:work", &renamed), Some(vec![(7, 11)]));
}

#[test]
fn user_me_uses_current_uid() {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        let mine = with_record(node(), |rec| rec.uid = Some(uid));
        let other = with_record(node(), |rec| rec.uid = Some(uid.wrapping_add(1)));
        assert!(hit("user:me", &mine));
        assert!(hit("user=ME", &mine));
        assert!(!hit("user:me", &other));
        assert!(hit("-user:me", &other));
        assert!(hit("user!=me", &other));
        assert!(!hit("user!=me", &mine));
    }
}

#[test]
fn states() {
    let z = with_sample(node(), |s| s.state = ProcState::Zombie);
    let t = with_sample(node(), |s| s.state = ProcState::Tracing);
    let stopped = with_sample(node(), |s| s.state = ProcState::Stopped);
    for q in [
        "state:Z",
        "state:z",
        "state=Z",
        "state:zombie",
        "state:zomb",
        "state:ZR",
        "status:defunct",
    ] {
        assert!(hit(q, &z), "{q}");
    }
    assert!(!hit("state:R", &z));
    assert!(hit("state!=R", &z));
    assert!(!hit("state!=Z", &z));
    assert!(hit("-state:R", &z));
    assert!(hit("state:t", &t) && hit("state:t", &stopped));
    assert!(hit("state:T", &stopped) && !hit("state:T", &t));
    assert!(hit("state:stopped", &stopped));
    assert!(hit(
        "state:running",
        &with_sample(node(), |s| s.state = ProcState::Running)
    ));
    assert!(hit(
        "state:sleeping",
        &with_sample(node(), |s| s.state = ProcState::Sleeping)
    ));
    assert!(hit(
        "state:D",
        &with_sample(node(), |s| s.state = ProcState::DiskSleep)
    ));
}

#[test]
fn numeric_comparisons() {
    let r = with_sample(node(), |s| {
        s.cpu_percent = 12.5;
        s.mem_percent = 2.5;
        s.mem_rss = 600 * 1024 * 1024;
        s.mem_virtual = 3 * 1024 * 1024 * 1024;
        s.threads = Some(8);
        s.nice = Some(-5);
        s.cpu_time_ms = 45_000;
        s.open_fds = Some(150);
    });
    for q in [
        "cpu>10",
        "cpu>=12.5",
        "cpu:12.5",
        "cpu=12.5%",
        "cpu<13",
        "cpu<=12.5",
        "cpu!=3",
        "mem>=2.5",
        "mem=2.5",
        "rss>500M",
        "rss>500MB",
        "rss>500mib",
        "rss<1G",
        "res>500m",
        "virt>=3G",
        "virt<1T",
        "threads>=8",
        "threads:8",
        "nice<0",
        "nice=-5",
        "pid=42",
        "pid:42",
        "ppid:1",
        "time>30s",
        "time<1m",
        "time>=45",
        "time=45000ms",
        "fds>100",
    ] {
        assert!(hit(q, &r), "{q}");
    }
    for q in [
        "cpu>20",
        "cpu=12",
        "mem>3",
        "rss>1G",
        "virt<3G",
        "threads>8",
        "nice>0",
        "pid=43",
        "ppid=2",
        "time>1m",
        "fds>200",
    ] {
        assert!(!hit(q, &r), "{q}");
    }
}

#[test]
fn float_cpu_values_compare_with_display_tolerance() {
    let r = with_sample(node(), |s| s.cpu_percent = 10.1);
    assert!(hit("cpu=10.1", &r));
    assert!(hit("cpu>=10.1", &r));
    assert!(hit("cpu<=10.1", &r));
}

#[test]
fn missing_numeric_values_never_match() {
    let r = node();
    assert!(!hit("threads>0", &r));
    assert!(!hit("threads<1000", &r));
    assert!(!hit("down>0", &r));
    assert!(!hit("threads!=3", &r));
    assert!(hit("-threads>0", &r));
}

#[test]
fn age_uses_start_time() {
    // now = 10_000s, start = 1_000s => age 9_000s = 2.5h.
    let r = node();
    for q in ["age>2h", "age<3h", "age>2h29m", "age<2h31m", "age>=9000"] {
        assert!(hit(q, &r), "{q}");
    }
    assert!(!hit("age>1d", &r));
    let unknown = with_record(node(), |rec| rec.key.start_time = 0);
    assert!(!hit("age>1s", &unknown) && !hit("age<1w", &unknown));
}

#[test]
fn counts_accept_decimal_suffixes() {
    let r = with_sample(node(), |s| s.ctx_switches = Some(25_000));
    assert!(hit("csw>20k", &r));
    assert!(!hit("csw>1m", &r));
}

#[test]
fn network_terms() {
    let r = with_net(node(), |n| {
        n.download_rate = 2 * 1024 * 1024;
        n.connections.push(ConnectionDetail {
            protocol: "TCP".into(),
            local_addr: "127.0.0.1".parse().unwrap(),
            local_port: 52_000,
            remote_addr: "1.1.1.1".parse().unwrap(),
            remote_port: 443,
            state: "ESTABLISHED".into(),
        });
    });
    assert!(hit("down>1M", &r));
    assert!(hit("down>=2M/s", &r));
    assert!(!hit("up>0", &r));
    assert!(hit("port:443", &r));
    assert!(hit("port:52000", &r));
    assert!(hit("port>50000", &r));
    assert!(!hit("port:80", &r));
    assert!(!hit("port:443", &node()));
    // `port!=N` is "not using port N": the ephemeral local end (52000 != 443)
    // must not make a connection to 443 match, and no connections means the
    // port is not in use.
    assert!(!hit("port!=443", &r));
    assert!(!hit("port!=52000", &r));
    assert!(hit("port!=80", &r));
    assert!(hit("port!=443", &node()));
    for q in ["port:443", "port:80", "port>50000"] {
        let negated = format!("-{q}");
        for row in [&r, &node()] {
            assert_ne!(hit(q, row), hit(&negated, row), "{q}");
        }
    }
    assert_eq!(hit("port!=443", &r), hit("-port:443", &r));
    assert!(hit("conn=1", &r));
    assert!(hit("is:net", &r));
    assert!(!hit("is:net", &node()));
    assert!(!hit("is:throttled", &r));
    let limited = with_net(node(), |n| {
        n.throttle_limit = Some(ThrottleLimit {
            download_limit: Some(1024),
            upload_limit: None,
            traffic_type: Default::default(),
        })
    });
    assert!(hit("is:throttled", &limited));
    assert!(hit("limit>=1K", &limited));
}

#[test]
fn traffic_mode_selects_counters() {
    let r = with_net(node(), |n| {
        n.download_rate = 100;
        n.internet_download_rate = 10;
    });
    let f = Filter::parse("down>50").unwrap();
    assert!(f.matches(&r, &ctx()).is_some());
    let internet = ColumnCtx {
        traffic: crate::process::TrafficType::Internet,
        ..ctx()
    };
    assert!(f.matches(&r, &internet).is_none());
}

#[test]
fn boolean_flags() {
    let mut ghost = node();
    ghost.exited_at_ms = Some(1);
    let kernel = with_record(node(), |rec| rec.kernel_thread = true);
    assert!(hit("is:ghost", &ghost) && !hit("is:ghost", &node()));
    assert!(hit("is:kernel", &kernel) && !hit("is:kernel", &node()));
    assert!(hit("-is:kernel", &node()));
    assert!(hit("IS:GHOST", &ghost));
}

#[test]
fn keys_are_case_insensitive_and_aliased() {
    let r = with_sample(node(), |s| s.cpu_percent = 50.0);
    assert!(hit("CPU>10", &r));
    assert!(hit("Name:node", &r));
    assert!(hit("args:server", &r));
}

#[test]
fn errors_are_friendly_with_spans() {
    let e = error("cpux>10");
    assert_eq!(e.message, "unknown field 'cpux' (try cpu)");
    assert_eq!(e.span, (0, 4));
    let e = error("node zzzzz>1");
    assert!(
        e.message
            .starts_with("unknown field 'zzzzz' (try cpu, mem, user, name"),
        "{}",
        e.message
    );
    assert_eq!(e.span, (5, 10));
    let e = error("cpu>");
    assert_eq!(e.message, "'cpu>' needs a value, e.g. cpu>10");
    assert_eq!(e.span, (0, 4));
    assert_eq!(error("rss>").message, "'rss>' needs a value, e.g. rss>500M");
    assert_eq!(
        error("user:").message,
        "'user:' needs a value, e.g. user:root"
    );
    let e = error("cpu>abc");
    assert_eq!(e.message, "'abc' isn't a number");
    assert_eq!(e.span, (4, 7));
    assert_eq!(
        error("rss>5Q").message,
        "unknown unit 'Q' (use K, M, G or T)"
    );
    assert_eq!(
        error("age>1y").message,
        "unknown unit 'y' (use s, m, h or d)"
    );
    assert_eq!(
        error("cpu>5x").message,
        "unknown unit 'x' (use a number or %)"
    );
    assert_eq!(
        error("threads>5q").message,
        "unknown unit 'q' (use k, m or g)"
    );
    let e = error("node \"abc");
    assert_eq!(e.message, "missing closing quote");
    assert_eq!(e.span, (5, 9));
    assert_eq!(error("cmd:\"abc").span, (4, 8));
    let e = error("| node");
    assert_eq!(e.span, (0, 1));
    assert!(e.message.contains('|'));
    assert_eq!(error("node |").span, (5, 6));
    assert_eq!(error("a | | b").span, (4, 5));
    let e = error("node -");
    assert_eq!(e.message, "'-' needs a term to exclude");
    assert_eq!(e.span, (5, 6));
    assert_eq!(error("- node").span, (0, 1));
    assert_eq!(error("\"\"").message, "empty quotes");
    assert_eq!(error("user>root").message, "use user: or user= (not >)");
    assert_eq!(error("user>root").span, (4, 5));
    assert!(error("state:q").message.starts_with("unknown state 'q'"));
    assert!(error("is:foo").message.starts_with("unknown flag 'foo'"));
    assert!(error("is>x").message.starts_with("use is:"));
    assert!(error("port:abc").message.contains("isn't a number"));
    assert!(error("cpu>-").message.contains("isn't a number"));
    assert!(error("age>1h-5m").message.contains("isn't a number"));
}

#[test]
fn suggestions_for_near_misses() {
    assert!(error("usr>root").message.contains("user"));
    assert!(error("thr0eads>1").message.contains("threads"));
    assert!(error("pr>1").message.contains("pri"));
    assert!(error("thread_count>1").message.starts_with("unknown field"));
    // With ':' an unknown key is searched as text; near misses keep a hint.
    let f = Filter::parse("usr:root").unwrap();
    assert!(
        f.hint().is_some_and(|h| h.contains("user:")),
        "{:?}",
        f.hint()
    );
    assert!(
        Filter::parse("pr:1")
            .unwrap()
            .hint()
            .unwrap()
            .contains("pri:")
    );
    assert_eq!(Filter::parse("localhost:8080").unwrap().hint(), None);
    assert_eq!(Filter::parse("cpu>1").unwrap().hint(), None);
}

#[test]
fn colon_words_are_plain_text() {
    let r = row(
        7,
        "node",
        &[
            "node",
            "--inspect=localhost:9229",
            "https://example.com/a",
            "node:internal",
        ],
    );
    assert!(hit("localhost:9229", &r));
    assert!(!hit("localhost:8080", &r));
    assert!(hit("https://example.com", &r));
    assert!(hit("node:internal", &r));
    assert!(hit("zzzzz:1", &row(1, "x", &["x", "--zzzzz:1"])));
    assert!(!hit("zzzzz:1", &r));
    assert!(hit("-localhost:8080", &r));
    // Literal substring only: no fuzzy subsequence match.
    assert!(!hit("lclhst:9229", &r));
    // Real keys still parse as fields.
    assert!(hit("name:node", &r));
    let e = error("localhost>8080");
    assert!(e.message.starts_with("unknown field 'localhost'"));
}

#[test]
fn spans_point_into_negated_and_later_terms() {
    assert_eq!(error("node -cpux>1").span, (6, 10));
    let q = "é cpu>zz";
    let e = error(q);
    assert_eq!(&q[e.span.0..e.span.1], "zz");
}

#[test]
fn merge_ranges_sorts_and_merges() {
    assert_eq!(
        merge_ranges(vec![(5, 7), (0, 2), (1, 3), (3, 4), (9, 9)]),
        vec![(0, 4), (5, 7)]
    );
    // Overlapping hits from separate terms merge.
    assert_eq!(m("serv rver", &node()), Some(vec![(20, 26)]));
}

#[test]
fn parse_value_units() {
    let s = (0, 0);
    assert_eq!(parse_value("1K", Unit::Bytes, s).unwrap(), 1024.0);
    assert_eq!(parse_value("1.5k", Unit::Bytes, s).unwrap(), 1536.0);
    assert_eq!(
        parse_value("2T", Unit::Bytes, s).unwrap(),
        2.0 * 1024f64.powi(4)
    );
    assert_eq!(parse_value("10b", Unit::Bytes, s).unwrap(), 10.0);
    assert_eq!(parse_value("1h30m", Unit::Seconds, s).unwrap(), 5400.0);
    assert_eq!(parse_value("2d", Unit::Seconds, s).unwrap(), 172_800.0);
    assert_eq!(parse_value("30s", Unit::Millis, s).unwrap(), 30_000.0);
    assert_eq!(parse_value("250ms", Unit::Millis, s).unwrap(), 250.0);
    assert_eq!(parse_value("1m", Unit::Millis, s).unwrap(), 60_000.0);
    assert_eq!(parse_value("-3", Unit::Count, s).unwrap(), -3.0);
    assert!(parse_value(".", Unit::Count, s).is_err());
    assert!(parse_value("1e999", Unit::Count, s).is_err());
}

#[test]
fn parse_never_panics_on_odd_input() {
    let inputs = [
        "\"",
        "-",
        "|",
        "--",
        "-\"\"",
        ":",
        "a:",
        "a:\"",
        "\\",
        "\"\\",
        "\"a\\\"",
        "é:1",
        "cpu>>1",
        "cpu=>1",
        "x|y|",
        "\u{0}",
        "-|",
        "a||b",
        "cpu<=",
        "\"a\"b\"",
        "user:me",
        "-\"x y\"",
        "name:\"\"",
        "\"\\\"",
        "ü-ü",
    ];
    for q in inputs {
        if let Ok(f) = Filter::parse(q) {
            let _ = f.matches(&node(), &ctx());
            let _ = f.matches(&row(1, "", &[]), &ctx());
        }
    }
}

#[test]
fn many_rows_match_quickly() {
    let rows: Vec<Row> = (0..2000)
        .map(|i| {
            row(
                i,
                &format!("proc{i}"),
                &[&format!("/usr/bin/proc{i}"), "--flag", "value"],
            )
        })
        .collect();
    let f = Filter::parse("prc1 | flag -user:root cpu>=0").unwrap();
    let start = std::time::Instant::now();
    for _ in 0..10 {
        let n = rows
            .iter()
            .filter(|r| f.matches(r, &ctx()).is_some())
            .count();
        assert_eq!(n, 2000);
    }
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}
