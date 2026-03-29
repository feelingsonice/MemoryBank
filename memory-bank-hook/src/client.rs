use memory_bank_protocol::IngestEnvelope;
use url::Url;

use crate::error::AppError;

pub fn build_ingest_url(server_url: &str) -> Result<Url, AppError> {
    let mut url = Url::parse(server_url).map_err(|err| {
        AppError::HttpClient(format!("Invalid ingest server URL '{server_url}': {err}"))
    })?;

    url.set_query(None);
    url.set_fragment(None);

    if !url.path().ends_with('/') {
        let mut path = url.path().to_owned();
        path.push('/');
        url.set_path(&path);
    }

    url.join("ingest").map_err(|err| {
        AppError::HttpClient(format!("Invalid ingest server URL '{server_url}': {err}"))
    })
}

pub fn post_ingest(server_url: &str, request: &IngestEnvelope) -> Result<(), AppError> {
    let url = build_ingest_url(server_url)?;
    match ureq::post(url.as_str()).send_json(request) {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, response)) => Err(AppError::HttpClient(format!(
            "Ingest request to {url} failed with status {code} {}",
            response.status_text()
        ))),
        Err(ureq::Error::Transport(error)) => Err(AppError::HttpClient(format!(
            "Failed to POST ingest request to {url}: {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{build_ingest_url, post_ingest};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use memory_bank_protocol::{
        ConversationFragment, ConversationScope, FragmentBody, INGEST_PROTOCOL_VERSION,
        IngestEnvelope, SourceMeta, Terminality,
    };
    use serde_json::json;

    #[test]
    fn build_ingest_url_appends_path_without_trailing_slash() {
        let url = build_ingest_url("http://127.0.0.1:8080").expect("url");
        assert_eq!(url.as_str(), "http://127.0.0.1:8080/ingest");
    }

    #[test]
    fn build_ingest_url_appends_path_with_trailing_slash() {
        let url = build_ingest_url("http://127.0.0.1:8080/").expect("url");
        assert_eq!(url.as_str(), "http://127.0.0.1:8080/ingest");
    }

    #[test]
    fn post_ingest_succeeds_on_accepted_response() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (server_url, task) = spawn_test_server(received.clone(), "202 Accepted");

        let request = sample_payload();
        let result = post_ingest(&server_url, &request);

        task.join().expect("server thread");
        assert!(result.is_ok());

        let requests = received.lock().expect("lock");
        assert_eq!(requests.as_slice(), &[request]);
    }

    #[test]
    fn post_ingest_returns_error_on_non_success_status() {
        let (server_url, task) =
            spawn_test_server(Arc::new(Mutex::new(Vec::new())), "400 Bad Request");

        let error =
            post_ingest(&server_url, &sample_payload()).expect_err("non-success should fail");

        task.join().expect("server thread");
        assert!(error.to_string().contains("status 400 Bad Request"));
    }

    #[test]
    fn request_uses_shared_protocol_payload() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let (server_url, task) = spawn_test_server(received.clone(), "202 Accepted");
        let payload = sample_payload();

        post_ingest(&server_url, &payload).expect("post");

        task.join().expect("server thread");

        let requests = received.lock().expect("lock");
        assert_eq!(requests[0], payload);
    }

    fn sample_payload() -> IngestEnvelope {
        IngestEnvelope {
            protocol_version: INGEST_PROTOCOL_VERSION,
            source: SourceMeta {
                agent: "claude-code".to_string(),
                event: "UserPromptSubmit".to_string(),
            },
            scope: ConversationScope {
                conversation_id: "session-1".to_string(),
                turn_id: None,
                fragment_id: "fragment-1".to_string(),
                sequence_hint: Some(1),
                emitted_at_rfc3339: Some("2026-03-05T00:00:00Z".to_string()),
            },
            fragment: ConversationFragment {
                terminality: Terminality::None,
                body: FragmentBody::UserMessage {
                    text: "hello".to_string(),
                },
            },
            raw: json!({"session_id": "session-1"}),
        }
    }

    fn spawn_test_server(
        received: Arc<Mutex<Vec<IngestEnvelope>>>,
        status: &str,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let status = status.to_string();
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let body = read_request_body(&stream);
            let request: IngestEnvelope = serde_json::from_slice(&body).expect("json");
            received.lock().expect("lock").push(request);

            let response =
                format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            stream.write_all(response.as_bytes()).expect("write");
            stream.flush().expect("flush");
        });

        (format!("http://{}", addr), task)
    }

    fn read_request_body(stream: &TcpStream) -> Vec<u8> {
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut content_length = 0usize;

        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).expect("read line");
            assert!(read > 0, "unexpected EOF while reading request headers");

            if line == "\r\n" {
                break;
            }

            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().expect("content length");
            }
        }

        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).expect("read body");
        body
    }
}
