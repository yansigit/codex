use super::*;
use http::HeaderValue;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;

#[tokio::test]
async fn explicit_no_retry_preserves_a_single_post_after_http2_refused_stream() {
    for retries in [true, false] {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut preface = [0; 24];
            socket.read_exact(&mut preface).unwrap();
            assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
            // Minimal HTTP/2 peer: SETTINGS, ACK peer settings, then refuse the
            // first stream before responding 200 to any replay. No HPACK values
            // are decoded or copied; this tests transport retry behavior only.
            socket.write_all(&[0, 0, 0, 4, 0, 0, 0, 0, 0]).unwrap();
            let mut submissions = 0;
            loop {
                let mut frame = [0; 9];
                if socket.read_exact(&mut frame).is_err() {
                    break;
                }
                let length = u32::from_be_bytes([0, frame[0], frame[1], frame[2]]) as usize;
                assert!(length <= 65536);
                let mut payload = vec![0; length];
                socket.read_exact(&mut payload).unwrap();
                if frame[3] == 4 && frame[4] & 1 == 0 {
                    socket.write_all(&[0, 0, 0, 4, 1, 0, 0, 0, 0]).unwrap();
                } else if frame[3] == 1 {
                    submissions += 1;
                    let mut response = if submissions == 1 {
                        vec![0, 0, 4, 3, 0]
                    } else {
                        vec![0, 0, 1, 1, 5]
                    };
                    response.extend_from_slice(&frame[5..9]);
                    if submissions == 1 {
                        response.extend_from_slice(&7u32.to_be_bytes());
                    } else {
                        response.push(0x88);
                    } // Static-table :status 200.
                    socket.write_all(&response).unwrap();
                    if submissions == 2 {
                        break;
                    }
                }
            }
            submissions
        });
        let mut builder = HttpClientBuilder::new()
            .http2_prior_knowledge()
            .without_request_logging();
        if !retries {
            builder = builder.without_automatic_retries();
        }
        let client = builder.build_direct().unwrap();
        let result = client
            .post(format!("http://{address}/upload"))
            .body("fixture")
            .send()
            .await;
        assert_eq!(result.is_ok(), retries);
        drop(result);
        drop(client);
        assert_eq!(server.join().unwrap(), if retries { 2 } else { 1 });
    }
}

#[tokio::test]
async fn custom_ca_fallback_preserves_builder_configuration() {
    let listener =
        std::net::TcpListener::bind(("127.0.0.1", 0)).expect("HTTP listener should bind");
    let address = listener
        .local_addr()
        .expect("HTTP listener should have an address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("HTTP listener should accept");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let bytes_read = stream.read(&mut chunk).expect("HTTP request should read");
            assert!(bytes_read > 0, "HTTP request should include headers");
            request.extend_from_slice(&chunk[..bytes_read]);
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .expect("HTTP listener should write response");
        String::from_utf8(request).expect("HTTP request should be UTF-8")
    });
    let mut headers = HeaderMap::new();
    headers.insert("x-builder-test", HeaderValue::from_static("preserved"));
    let client = HttpClientBuilder::new()
        .default_headers(headers)
        .build_with_custom_ca_fallback_using(ProxyRouting::Direct, |_| {
            Err(BuildCustomCaTransportError::InvalidCaFile {
                source_env: "TEST_CA_ENV",
                path: PathBuf::from("invalid-test-ca.pem"),
                detail: "synthetic invalid CA".to_string(),
            })
        });

    let client = HttpClient {
        backend: HttpClientBackend::Direct(client),
    };

    let response = client
        .get(format!("http://{address}/fallback"))
        .send()
        .await
        .expect("fallback client should send request");
    assert!(response.status().is_success());
    let request = server.join().expect("HTTP listener should finish");
    assert!(
        request
            .lines()
            .any(|line| line.eq_ignore_ascii_case("x-builder-test: preserved"))
    );
}

/// A server cannot keep a connection reusable after the caller requests closure.
#[tokio::test]
async fn connection_close_prevents_reuse_when_server_ignores_it()
-> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let url = format!("http://{}/", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await?);
        }
        // Deliberately ignore the request's Connection: close header.
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";
        stream.write_all(response).await?;
        let mut byte = [0];
        let reused = tokio::select! {
            read = stream.read(&mut byte) => {
                if read? == 0 {
                    stream = listener.accept().await?.0;
                    false
                } else {
                    true
                }
            }
            accepted = listener.accept() => {
                stream = accepted?.0;
                false
            }
        };
        headers.clear();
        if reused {
            headers.push(byte[0]);
        }
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(stream.read_u8().await?);
        }
        stream.write_all(response).await?;
        Ok::<bool, std::io::Error>(reused)
    });
    let client = HttpClientBuilder::new().build_direct()?;
    for _ in 0..2 {
        client
            .get(&url)
            .header(http::header::CONNECTION, HeaderValue::from_static("close"))
            .send()
            .await?
            .bytes()
            .await?;
    }
    assert!(
        !server.await??,
        "the server reused a connection marked closed"
    );
    Ok(())
}
