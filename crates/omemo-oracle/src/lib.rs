//! Test harness: runs the python-omemo reference device (`tests/oracle/oracle.py`) as a
//! subprocess and talks JSON lines to it. Only for tests of the crates in this workspace.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
pub use serde_json::{Value, json};

pub fn b64(data: &[u8]) -> String {
    STANDARD.encode(data)
}

pub fn unb64(value: &Value) -> Vec<u8> {
    STANDARD.decode(value.as_str().expect("base64 string")).unwrap()
}

pub fn key32(value: &Value) -> [u8; 32] {
    unb64(value).try_into().unwrap()
}

/// The JID the oracle uses for itself.
pub const ORACLE_JID: &str = "oracle@omtalk.test";

pub struct Oracle {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    pub device_id: u32,
}

impl Oracle {
    /// Starts a fresh reference device with new keys. Fails, rather than skips, when the
    /// environment is missing: a test that silently did not compare proves nothing.
    pub fn spawn() -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/oracle");
        let python = dir.join(".venv/bin/python");
        assert!(
            python.exists(),
            "oracle environment missing: run `mise run setup`"
        );
        let mut child = Command::new(python)
            .arg("-I")
            .arg(dir.join("oracle.py"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut oracle = Self {
            child,
            stdin,
            stdout,
            device_id: 0,
        };
        oracle.device_id = oracle.call(json!({"cmd": "start"}))["device_id"]
            .as_u64()
            .unwrap() as u32;
        oracle
    }

    /// Sends one command; `Err` carries the exception the oracle raised.
    pub fn try_call(&mut self, request: Value) -> Result<Value, String> {
        writeln!(self.stdin, "{request}").unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let answer: Value = serde_json::from_str(&line).expect("oracle answered with JSON");
        match answer.get("error") {
            Some(error) => Err(error.as_str().unwrap().to_owned()),
            None => Ok(answer),
        }
    }

    pub fn call(&mut self, request: Value) -> Value {
        self.try_call(request).unwrap()
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
