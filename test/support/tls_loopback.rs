//! Real, certificate-verifying TLS peers for unit and GNU oracle tests.
//! The fixture private key is public test data and has no deployment use.
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

pub fn resources() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test/lisp/net/tls-loopback-resources")
}

pub struct TlsPeer {
    pub port: u16,
    stop: std::sync::mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TlsPeer {
    pub fn spawn(connections: usize, delay: Duration, echo: bool) -> Self {
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_file(resources().join("server.pem")).unwrap()],
                PrivateKeyDer::from_pem_file(resources().join("server-key.pem")).unwrap(),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let config = Arc::new(config);
            for _ in 0..connections {
                // GNU and Neomacs oracle processes bootstrap separately;
                // cancellation below keeps failed tests from waiting this long.
                let deadline = Instant::now() + Duration::from_secs(30);
                let socket = loop {
                    if stopped.try_recv().is_ok() {
                        return;
                    }
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(_) => return,
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                thread::sleep(delay);
                let mut stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(config.clone()).unwrap(),
                    socket,
                );
                if echo {
                    let mut input = [0; 128];
                    if let Ok(count) = stream.read(&mut input) {
                        let _ = stream.write_all(&input[..count]);
                    }
                } else {
                    let _ = stream.write_all(b"verified loopback\n");
                }
                let _ = stream.flush();
            }
        });
        Self {
            port,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for TlsPeer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            // Cleanup must not replace an assertion failure or panic again
            // while the test is already unwinding.
            let _ = worker.join();
        }
    }
}

/// A fixed-length encrypted echo whose lifetime is controlled by its caller.
/// Unlike the one-read peer, this fixture verifies the complete application
/// message and keeps the transport open until the Lisp client is cleaned up.
/// All socket/TLS state belongs to the worker; cancellation crosses threads
/// only through the channel. Failed clients cannot leave an unbounded join.
pub struct FramedEchoPeer {
    pub port: u16,
    stop: std::sync::mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<Result<(), String>>>,
}

impl FramedEchoPeer {
    pub fn spawn(expected: &'static [u8], delay: Duration) -> Self {
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_file(resources().join("server.pem")).unwrap()],
                PrivateKeyDer::from_pem_file(resources().join("server-key.pem")).unwrap(),
            )
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let socket = loop {
                if stopped.try_recv().is_ok() {
                    return Err("TLS client cancelled before connecting".to_owned());
                }
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => return Err(format!("accept framed TLS client: {error}")),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .map_err(|error| format!("set TLS read timeout: {error}"))?;
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .map_err(|error| format!("set TLS write timeout: {error}"))?;
            thread::sleep(delay);
            let connection = rustls::ServerConnection::new(Arc::new(config))
                .map_err(|error| format!("create framed TLS server: {error}"))?;
            let mut stream = rustls::StreamOwned::new(connection, socket);
            let mut received = vec![0; expected.len()];
            stream
                .read_exact(&mut received)
                .map_err(|error| format!("read framed TLS message: {error}"))?;
            if received != expected {
                return Err(format!("unexpected framed TLS message: {received:?}"));
            }
            stream
                .write_all(&received)
                .and_then(|()| stream.flush())
                .map_err(|error| format!("echo framed TLS message: {error}"))?;
            stopped
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| format!("TLS client did not finish after echo: {error}"))?;
            Ok(())
        });
        Self {
            port,
            stop,
            worker: Some(worker),
        }
    }

    /// Cancel the controlled lifetime and report protocol/transport failures.
    pub fn finish(mut self) -> Result<(), String> {
        let _ = self.stop.send(());
        self.worker
            .take()
            .ok_or_else(|| "TLS worker already joined".to_owned())?
            .join()
            .map_err(|_| "framed TLS worker panicked".to_owned())?
    }
}

impl Drop for FramedEchoPeer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            // Explicit finish reports failures; cleanup must not panic while
            // another test assertion is unwinding.
            let _ = worker.join();
        }
    }
}

pub struct StalledPeer {
    pub port: u16,
    stop: std::sync::mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<()>>,
}

impl StalledPeer {
    pub fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if stopped.try_recv().is_ok() {
                    return;
                }
                match listener.accept() {
                    Ok((socket, _)) => {
                        // Bound the defective blocking path without adding a
                        // timeout to the production TLS implementation.
                        let _ = stopped.recv_timeout(Duration::from_secs(1));
                        drop(socket);
                        return;
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => return,
                }
            }
        });
        Self {
            port,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for StalledPeer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            // Cleanup must not replace an assertion failure or panic again
            // while the test is already unwinding.
            let _ = worker.join();
        }
    }
}
