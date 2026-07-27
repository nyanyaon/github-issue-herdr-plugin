//! The test harness: everything below the app seam is real.
//!
//! A real `git` against a real fixture repo in a temp dir, the real HTTP client,
//! the real renderer. The only stub is the GitHub endpoint itself — a local
//! server returning canned GraphQL responses, gzipped, which is how the tests
//! exercise the client's gzip path end to end.

// Every test binary compiles the whole harness, and no single one uses all of it.
#![allow(dead_code)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::write::GzEncoder;
use herdr_issues::app::App;
use herdr_issues::environment::Environment;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// A local stand-in for `api.github.com/graphql`.
pub struct StubGithub {
    pub url: String,
    /// Every request body it has been sent, in order — which is how a test
    /// asserts both what was asked for and that nothing was asked at all.
    requests: Arc<Mutex<Vec<String>>>,
}

impl StubGithub {
    /// Answers every request with `200` and the given GraphQL body.
    pub fn serving(body: impl Into<String>) -> Self {
        Self::answering(200, body.into(), false)
    }

    /// Answers every request with the given status and body.
    pub fn responding(status: u16, body: impl Into<String>) -> Self {
        Self::answering(status, body.into(), false)
    }

    /// Answers the first request with the given body and every later one with
    /// `503` — how a test makes a refresh fail after a start that succeeded.
    pub fn serving_once(body: impl Into<String>) -> Self {
        Self::answering(200, body.into(), true)
    }

    fn answering(status: u16, body: String, only_once: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub server");
        let url = format!(
            "http://{}/graphql",
            listener.local_addr().expect("stub server address")
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        thread::spawn(move || {
            for (served, stream) in listener.incoming().enumerate() {
                let Ok(stream) = stream else { continue };
                if only_once && served > 0 {
                    answer(stream, 503, "{}", &recorded);
                } else {
                    answer(stream, status, &body, &recorded);
                }
            }
        });
        Self { url, requests }
    }

    /// How many requests the viewer has issued so far.
    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("stub request log").len()
    }

    /// The body of the request at `index`, as sent.
    pub fn request(&self, index: usize) -> String {
        self.requests.lock().expect("stub request log")[index].clone()
    }
}

fn answer(mut stream: TcpStream, status: u16, body: &str, requests: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stub connection"));
    let mut content_length = 0usize;
    let mut wants_gzip = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        let lowered = line.to_ascii_lowercase();
        if let Some(value) = lowered.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        if let Some(value) = lowered.strip_prefix("accept-encoding:") {
            wants_gzip = value.contains("gzip");
        }
    }
    let mut request_body = vec![0u8; content_length];
    let _ = reader.read_exact(&mut request_body);
    // Recorded before the answer goes out, so a test that sees the response has
    // already seen the request.
    requests
        .lock()
        .expect("stub request log")
        .push(String::from_utf8_lossy(&request_body).into_owned());

    let (payload, encoding) = if wants_gzip {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(body.as_bytes()).expect("gzip the body");
        (encoder.finish().expect("finish gzip"), "gzip")
    } else {
        (body.as_bytes().to_vec(), "identity")
    };

    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Encoding: {encoding}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&payload);
    let _ = stream.flush();
}

/// A real git repo in a temp directory.
pub struct FixtureRepo {
    pub path: PathBuf,
}

impl FixtureRepo {
    /// A repo whose only remote is `origin`, pointing at `url`.
    pub fn with_origin(url: &str) -> Self {
        let repo = Self::empty();
        repo.git(&["remote", "add", "origin", url]);
        repo
    }

    /// A git repo with no remotes at all.
    pub fn empty() -> Self {
        let path = temp_dir("fixture-repo");
        fs::create_dir_all(&path).expect("create fixture repo");
        let repo = Self { path };
        repo.git(&["init", "--quiet", "-b", "main"]);
        repo
    }

    /// A directory that is not a git repo.
    pub fn not_a_repo() -> PathBuf {
        let path = temp_dir("not-a-repo");
        fs::create_dir_all(&path).expect("create non-repo directory");
        path
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }
}

impl Drop for FixtureRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = format!(
        "herdr-issues-{prefix}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    std::env::temp_dir().join(unique)
}

/// The environment description an app is constructed from in tests: a real
/// workspace directory, the stub endpoint, a token that is never checked.
pub fn environment(workspace_cwd: &Path, stub: &StubGithub) -> Environment {
    Environment {
        workspace_cwd: workspace_cwd.to_path_buf(),
        graphql_url: stub.url.clone(),
        token: Some("test-token".to_string()),
    }
}

/// Renders the app into a terminal test backend and returns the text on screen.
pub fn screen(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test terminal");
    terminal
        .draw(|frame| app.render(frame))
        .expect("draw the pane");
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            let line: String = (0..buffer.area.width)
                .filter_map(|x| buffer.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect();
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// An RFC 3339 timestamp that many seconds in the past, so a rendered age is
/// deterministic.
pub fn seconds_ago(seconds: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs() as i64;
    format_timestamp(now - seconds)
}

fn format_timestamp(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60
    )
}

/// The inverse of the viewer's `days_from_civil`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}
