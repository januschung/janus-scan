//! HTTP and event-stream helpers shared by the server tests.
#![allow(dead_code)]

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;


pub async fn call(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    (status, resp.into_body().collect().await.unwrap().to_bytes().to_vec())
}

pub async fn json_call(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let (status, bytes) = call(app, method, uri, body).await;
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

pub fn scan_body() -> Value {
    json!({ "request": { "source": "feeder", "duplex": false, "dpi": 300, "color": "color", "paper": { "kind": "letter" } } })
}

/// Reads `event:` / `data:` blocks from `/api/events`.
pub struct Events {
    body: Body,
    buf: String,
}

impl Events {
    pub async fn open(app: &Router) -> Self {
        let resp = app.clone().oneshot(Request::get("/api/events").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        Self { body: resp.into_body(), buf: String::new() }
    }

    pub async fn next(&mut self) -> (String, Value) {
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..end + 2).collect();
                let (mut name, mut data) = (None, String::new());
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        name = Some(v.trim().to_string());
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push_str(v.trim_start());
                    }
                }
                match name {
                    Some(name) => return (name, serde_json::from_str(&data).unwrap()),
                    None => continue, // keep-alive comment
                }
            }
            let frame = tokio::time::timeout(Duration::from_secs(10), self.body.frame())
                .await
                .expect("no event within 10 s")
                .expect("event stream ended")
                .unwrap();
            if let Ok(data) = frame.into_data() {
                self.buf.push_str(std::str::from_utf8(&data).unwrap());
            }
        }
    }

    /// Next event with this name, skipping others.
    pub async fn next_named(&mut self, want: &str) -> Value {
        loop {
            let (name, v) = self.next().await;
            if name == want {
                return v;
            }
        }
    }

    /// Scan events up to and including the one that ends the pass.
    pub async fn scan_pass(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        loop {
            let v = self.next_named("scan").await;
            let end = matches!(v["kind"].as_str(), Some("feeder-empty" | "done" | "error"));
            out.push(v);
            if end {
                return out;
            }
        }
    }

    /// Waits until the scanner lock is released.
    pub async fn idle(&mut self) {
        while self.next_named("state").await["scanning"] != json!(false) {}
    }
}

pub fn kinds(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["kind"].as_str().unwrap()).collect()
}
