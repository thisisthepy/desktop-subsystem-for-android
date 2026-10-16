//! The local API: the service that answers the apps in users' sessions, and the client
//! those apps use.
//!
//! The service takes each caller's user from the transport (the operating system reports
//! it) and acts on that user's Android only. Nothing a client sends can name another user.
//! A connection that breaks the rules is closed; the service and its other connections go
//! on.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_protocol::host_api::types::{
    self, envelope::Body, request, response, Envelope, ErrorCode, Request, Response, Role,
};
use dsa_protocol::host_api::{
    client_connect, decode, local_hello, send, service_accept, HandshakeError,
};
use dsa_transport::local::{LocalListener, Peer};
use dsa_transport::{Connection, Connector, TransportError};

use crate::session::{Session, SessionState};

/// One user's Android, as the service drives it.
pub trait UserAndroid: Send + Sync {
    fn state(&self) -> SessionState;
    /// Why it last stopped, in words a person can act on.
    fn last_reason(&self) -> Option<String>;
    fn start(&self);
    fn stop(&self);
}

impl UserAndroid for Session {
    fn state(&self) -> SessionState {
        Session::state(self)
    }
    fn last_reason(&self) -> Option<String> {
        Session::last_reason(self).map(|reason| reason.to_string())
    }
    fn start(&self) {
        Session::start(self)
    }
    fn stop(&self) {
        Session::stop(self)
    }
}

/// Finds or creates the Android of a user.
pub trait UserAndroids: Send + Sync {
    /// The Android of `user` (an identity as the transport reports it), created on first
    /// use. An error says why this user cannot have one.
    fn android(&self, user: &str) -> Result<Arc<dyn UserAndroid>, String>;
}

/// One Android per user, each created by `create` the first time its user asks.
pub struct PerUser<F> {
    create: F,
    androids: Mutex<HashMap<String, Arc<dyn UserAndroid>>>,
}

impl<F> PerUser<F>
where
    F: Fn(&str) -> Result<Arc<dyn UserAndroid>, String> + Send + Sync,
{
    pub fn new(create: F) -> PerUser<F> {
        PerUser {
            create,
            androids: Mutex::new(HashMap::new()),
        }
    }
}

impl<F> PerUser<F> {
    /// Stops every user's Android and waits at most `timeout` for all of them. Returns the
    /// users whose Android had not stopped by then, with its state.
    pub fn stop_all(&self, timeout: Duration) -> Vec<(String, SessionState)> {
        let androids: Vec<(String, Arc<dyn UserAndroid>)> = self
            .androids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(user, android)| (user.clone(), Arc::clone(android)))
            .collect();
        for (_, android) in &androids {
            android.stop();
        }
        let deadline = Instant::now() + timeout;
        loop {
            let running: Vec<(String, SessionState)> = androids
                .iter()
                .map(|(user, android)| (user.clone(), android.state()))
                .filter(|(_, state)| *state != SessionState::Stopped)
                .collect();
            if running.is_empty() || Instant::now() >= deadline {
                return running;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl<F> UserAndroids for PerUser<F>
where
    F: Fn(&str) -> Result<Arc<dyn UserAndroid>, String> + Send + Sync,
{
    fn android(&self, user: &str) -> Result<Arc<dyn UserAndroid>, String> {
        let mut androids = self.androids.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(android) = androids.get(user) {
            return Ok(Arc::clone(android));
        }
        let android = (self.create)(user)?;
        androids.insert(user.to_string(), Arc::clone(&android));
        Ok(android)
    }
}

fn to_wire(state: SessionState) -> types::SessionState {
    match state {
        SessionState::Stopped => types::SessionState::Stopped,
        SessionState::Starting => types::SessionState::Starting,
        SessionState::Active => types::SessionState::Active,
        SessionState::Dozing => types::SessionState::Dozing,
        SessionState::Frozen => types::SessionState::Frozen,
        SessionState::Stopping => types::SessionState::Stopping,
    }
}

fn from_wire(state: i32) -> Option<SessionState> {
    match types::SessionState::try_from(state).ok()? {
        types::SessionState::Unspecified => None,
        types::SessionState::Stopped => Some(SessionState::Stopped),
        types::SessionState::Starting => Some(SessionState::Starting),
        types::SessionState::Active => Some(SessionState::Active),
        types::SessionState::Dozing => Some(SessionState::Dozing),
        types::SessionState::Frozen => Some(SessionState::Frozen),
        types::SessionState::Stopping => Some(SessionState::Stopping),
    }
}

/// The service end of the local API.
pub struct HostApiService {
    androids: Arc<dyn UserAndroids>,
    hello: types::Hello,
    handshake_timeout: Duration,
}

impl HostApiService {
    pub fn new(androids: Arc<dyn UserAndroids>, build: &str) -> HostApiService {
        HostApiService {
            androids,
            hello: local_hello(Role::Service, build),
            handshake_timeout: Duration::from_secs(5),
        }
    }

    /// Accepts connections until `stop` is set, serving each on its own thread. A failed
    /// accept is logged and the loop goes on; only `stop` ends it.
    pub fn serve(self: Arc<Self>, mut listener: Box<dyn LocalListener>, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept(Duration::from_millis(200)) {
                Ok((connection, peer)) => {
                    let service = Arc::clone(&self);
                    let spawned = thread::Builder::new()
                        .name("dsa-host-api".into())
                        .spawn(move || service.serve_connection(connection, peer));
                    if let Err(error) = spawned {
                        eprintln!("local API: could not start a connection thread: {error}");
                    }
                }
                Err(TransportError::Timeout) => {}
                Err(error) => {
                    eprintln!("local API: accepting a connection failed: {error}");
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    /// Serves one client until it disconnects or breaks the protocol.
    pub fn serve_connection(&self, mut connection: Box<dyn Connection>, peer: Peer) {
        if let Err(error) = service_accept(&mut *connection, &self.hello, self.handshake_timeout) {
            eprintln!("local API: refused a client of {}: {error}", peer.user);
            return;
        }
        loop {
            let frame = match connection.recv(None) {
                Ok(frame) => frame,
                Err(_) => return,
            };
            let request = match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::Request(request)),
                }) => request,
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something other than a request; \
                         closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame ({error}); \
                         closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
            };
            let response = self.answer(&peer, request);
            if send(&mut *connection, Body::Response(response)).is_err() {
                return;
            }
        }
    }

    fn answer(&self, peer: &Peer, request: Request) -> Response {
        let body = match request.body {
            None => error(
                ErrorCode::UnknownRequest,
                "the request is empty or of a kind this service does not know; \
                 update the service"
                    .into(),
            ),
            Some(body) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => {
                    match body {
                        request::Body::GetStatus(_) => {}
                        request::Body::StartSession(_) => android.start(),
                        request::Body::StopSession(_) => android.stop(),
                    }
                    response::Body::Status(types::Status {
                        state: to_wire(android.state()) as i32,
                        last_reason: android.last_reason().unwrap_or_default(),
                    })
                }
            },
        };
        Response {
            id: request.id,
            body: Some(body),
        }
    }
}

fn error(code: ErrorCode, detail: String) -> response::Body {
    response::Body::Error(types::Error {
        code: code as i32,
        detail,
    })
}

/// The caller's Android as the service reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: SessionState,
    pub last_reason: Option<String>,
}

/// Why a local API call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// Connecting or the handshake failed; the detail says why.
    Connect(HandshakeError),
    /// The connection broke or timed out.
    Transport(TransportError),
    /// The service answered with an error.
    Service { code: ErrorCode, detail: String },
    /// The service's answer did not make sense.
    Malformed(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Connect(error) => write!(f, "{error}"),
            ClientError::Transport(error) => write!(f, "the DSA service: {error}"),
            ClientError::Service { detail, .. } => write!(f, "the DSA service: {detail}"),
            ClientError::Malformed(why) => write!(f, "the DSA service answered oddly: {why}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// The client end of the local API.
pub struct HostApiClient {
    connection: Box<dyn Connection>,
    next_id: u64,
    timeout: Duration,
}

impl HostApiClient {
    /// Connects through `connector` and completes the handshake, waiting at most `timeout`
    /// for each step.
    pub fn connect(
        connector: &mut dyn Connector,
        build: &str,
        timeout: Duration,
    ) -> Result<HostApiClient, ClientError> {
        let mut connection = connector
            .connect(timeout)
            .map_err(|error| ClientError::Connect(HandshakeError::Transport(error)))?;
        client_connect(&mut *connection, &local_hello(Role::Client, build), timeout)
            .map_err(ClientError::Connect)?;
        Ok(HostApiClient {
            connection,
            next_id: 1,
            timeout,
        })
    }

    pub fn status(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::GetStatus(types::GetStatus {}))
    }

    /// Asks for the caller's Android to start. Returns the state right after the request.
    pub fn start(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::StartSession(types::StartSession {}))
    }

    /// Asks for the caller's Android to stop. Returns the state right after the request.
    pub fn stop(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::StopSession(types::StopSession {}))
    }

    fn call(&mut self, body: request::Body) -> Result<Status, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        send(
            &mut *self.connection,
            Body::Request(Request {
                id,
                body: Some(body),
            }),
        )
        .map_err(ClientError::Transport)?;
        let frame = self
            .connection
            .recv(Some(self.timeout))
            .map_err(ClientError::Transport)?;
        let response = match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::Response(response)),
            }) => response,
            Ok(_) => return Err(ClientError::Malformed("expected a response".into())),
            Err(error) => return Err(ClientError::Malformed(error.to_string())),
        };
        if response.id != id {
            return Err(ClientError::Malformed(format!(
                "the answer is for request {}, expected {id}",
                response.id
            )));
        }
        match response.body {
            Some(response::Body::Status(status)) => Ok(Status {
                state: from_wire(status.state).ok_or_else(|| {
                    ClientError::Malformed(format!("unknown session state {}", status.state))
                })?,
                last_reason: Some(status.last_reason).filter(|reason| !reason.is_empty()),
            }),
            Some(response::Body::Error(error)) => Err(ClientError::Service {
                code: ErrorCode::try_from(error.code).unwrap_or(ErrorCode::Unspecified),
                detail: error.detail,
            }),
            None => Err(ClientError::Malformed("an empty response".into())),
        }
    }
}
