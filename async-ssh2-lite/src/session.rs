use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

use ssh2::{
    BlockDirections, DisconnectCode, Error as Ssh2Error, HashType, HostKeyType,
    KeyboardInteractivePrompt, KnownHosts, MethodType, ScpFileStat, Session,
};

use crate::{
    agent::AsyncAgent, channel::AsyncChannel, error::Error, listener::AsyncListener,
    session_stream::AsyncSessionStream, sftp::AsyncSftp,
};

pub struct AsyncSession<S> {
    inner: Session,
    stream: Arc<S>,
}

#[cfg(unix)]
impl<S> AsyncSession<S>
where
    S: AsRawFd + 'static,
{
    pub fn new(stream: S, configuration: Option<SessionConfiguration>) -> io::Result<Self> {
        let mut session = get_session(configuration)?;
        session.set_tcp_stream(stream.as_raw_fd());

        let stream = Arc::new(stream);

        Ok(Self {
            inner: session,
            stream,
        })
    }
}

#[cfg(windows)]
impl<S> AsyncSession<S>
where
    S: AsRawSocket + 'static,
{
    pub fn new(stream: S, configuration: Option<SessionConfiguration>) -> io::Result<Self> {
        let mut session = get_session(configuration)?;
        session.set_tcp_stream(stream.as_raw_socket());

        let stream = Arc::new(stream);

        Ok(Self {
            inner: session,
            stream,
        })
    }
}

impl<S> AsyncSession<S> {
    pub fn is_blocking(&self) -> bool {
        self.inner.is_blocking()
    }

    pub fn banner(&self) -> Option<&str> {
        self.inner.banner()
    }

    pub fn banner_bytes(&self) -> Option<&[u8]> {
        self.inner.banner_bytes()
    }

    pub fn timeout(&self) -> u32 {
        self.inner.timeout()
    }
}

impl<S> AsyncSession<S>
where
    S: AsyncSessionStream + Send + Sync,
{
    pub async fn handshake(&mut self) -> Result<(), Error> {
        let sess = self.inner.clone();
        let inner = &mut self.inner;

        self.stream
            .read_and_write_with(&sess, || inner.handshake())
            .await
    }

    pub async fn userauth_password(&self, username: &str, password: &str) -> Result<(), Error> {
        self.stream
            .read_and_write_with(&self.inner, || {
                self.inner.userauth_password(username, password)
            })
            .await
    }

    pub async fn userauth_keyboard_interactive<P: KeyboardInteractivePrompt + Send>(
        &self,
        username: &str,
        prompter: &mut P,
    ) -> Result<(), Error> {
        self.stream
            .read_and_write_with(&self.inner, || {
                self.inner.userauth_keyboard_interactive(username, prompter)
            })
            .await
    }

    pub async fn userauth_agent(&self, username: &str) -> io::Result<()> {
        let mut agent = self.agent()?;
        agent.connect().await?;
        agent.list_identities().await?;
        let identities = agent.identities()?;
        let identity = match identities.get(0) {
            Some(identity) => identity,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "no identities found in the ssh agent",
                ))
            }
        };
        agent.userauth(username, identity).await
    }

    pub async fn userauth_pubkey_file(
        &self,
        username: &str,
        pubkey: Option<&Path>,
        privatekey: &Path,
        passphrase: Option<&str>,
    ) -> Result<(), Error> {
        self.stream
            .read_and_write_with(&self.inner, || {
                self.inner
                    .userauth_pubkey_file(username, pubkey, privatekey, passphrase)
            })
            .await
    }

    #[cfg(unix)]
    pub async fn userauth_pubkey_memory(
        &self,
        username: &str,
        pubkeydata: Option<&str>,
        privatekeydata: &str,
        passphrase: Option<&str>,
    ) -> Result<(), Error> {
        self.stream
            .read_and_write_with(&self.inner, || {
                self.inner
                    .userauth_pubkey_memory(username, pubkeydata, privatekeydata, passphrase)
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn userauth_hostbased_file(
        &self,
        username: &str,
        publickey: &Path,
        privatekey: &Path,
        passphrase: Option<&str>,
        hostname: &str,
        local_username: Option<&str>,
    ) -> Result<(), Error> {
        self.stream
            .read_and_write_with(&self.inner, || {
                self.inner
                    .userauth_hostbased_file(
                        username,
                        publickey,
                        privatekey,
                        passphrase,
                        hostname,
                        local_username,
                    )
                    .map_err(Into::into)
            })
            .await
    }

    pub fn authenticated(&self) -> bool {
        self.inner.authenticated()
    }

    pub async fn auth_methods(&self, username: &str) -> Result<&str, Error> {
        let inner = &self.inner;

        self.stream
            .read_and_write_with(&self.inner, || inner.auth_methods(username))
            .await
    }

    pub async fn method_pref(&self, method_type: MethodType, prefs: &str) -> Result<(), Error> {
        self.stream
            .read_and_write_with(&self.inner, || self.inner.method_pref(method_type, prefs))
            .await
    }

    pub fn methods(&self, method_type: MethodType) -> Option<&str> {
        self.inner.methods(method_type)
    }

    pub async fn supported_algs(
        &self,
        method_type: MethodType,
    ) -> Result<Vec<&'static str>, Error> {
        self.stream
            .read_and_write_with(&self.inner, || self.inner.supported_algs(method_type))
            .await
    }

    pub fn agent(&self) -> io::Result<AsyncAgent<S>> {
        let ret = self.inner.agent();

        // ret.map(|agent| AsyncAgent::from_parts(agent, self.stream.clone()))
        todo!()
    }

    pub fn known_hosts(&self) -> io::Result<KnownHosts> {
        self.inner.known_hosts().map_err(Into::into)
    }

    pub async fn channel_session(&self) -> io::Result<AsyncChannel<S>> {
        let ret = self
            .stream
            .read_and_write_with(&self.inner, || self.inner.channel_session())
            .await;

        // ret.map(|channel| AsyncChannel::from_parts(channel, self.stream.clone()))
        todo!()
    }

    pub async fn channel_direct_tcpip(
        &self,
        host: &str,
        port: u16,
        src: Option<(&str, u16)>,
    ) -> io::Result<AsyncChannel<S>> {
        let ret = self
            .stream
            .read_and_write_with(&self.inner, || {
                self.inner.channel_direct_tcpip(host, port, src)
            })
            .await;

        // ret.map(|channel| AsyncChannel::from_parts(channel, self.stream.clone()))
        todo!()
    }

    pub async fn channel_forward_listen(
        &self,
        remote_port: u16,
        host: Option<&str>,
        queue_maxsize: Option<u32>,
    ) -> io::Result<(AsyncListener<S>, u16)> {
        let inner = &self.inner;

        let ret = self
            .stream
            .read_and_write_with(&self.inner, || {
                inner.channel_forward_listen(remote_port, host, queue_maxsize)
            })
            .await;

        // ret.map(|(listener, port)| {
        //     (
        //         AsyncListener::from_parts(listener, self.stream.clone()),
        //         port,
        //     )
        // })
        todo!()
    }

    pub async fn scp_recv(&self, path: &Path) -> io::Result<(AsyncChannel<S>, ScpFileStat)> {
        let ret = self
            .stream
            .read_and_write_with(&self.inner, || self.inner.scp_recv(path))
            .await;

        // ret.map(|(channel, scp_file_stat)| {
        //     (
        //         AsyncChannel::from_parts(channel, self.stream.clone()),
        //         scp_file_stat,
        //     )
        // })
        todo!()
    }

    pub async fn scp_send(
        &self,
        remote_path: &Path,
        mode: i32,
        size: u64,
        times: Option<(u64, u64)>,
    ) -> io::Result<AsyncChannel<S>> {
        let ret = self
            .stream
            .read_and_write_with(&self.inner, || {
                self.inner.scp_send(remote_path, mode, size, times)
            })
            .await;

        // ret.map(|channel| AsyncChannel::from_parts(channel, self.stream.clone()))
        todo!()
    }

    pub async fn sftp(&self) -> io::Result<AsyncSftp<S>> {
        let ret = self
            .stream
            .read_and_write_with(&self.inner, || self.inner.sftp().map_err(Into::into))
            .await;

        // ret.map(|sftp| AsyncSftp::from_parts(sftp, self.stream.clone()))
        todo!()
    }

    pub async fn channel_open(
        &self,
        channel_type: &str,
        window_size: u32,
        packet_size: u32,
        message: Option<&str>,
    ) -> io::Result<AsyncChannel<S>> {
        let inner = &self.inner;

        let ret = self
            .stream
            .read_and_write_with(&self.inner, || {
                inner.channel_open(channel_type, window_size, packet_size, message)
            })
            .await;

        // ret.map(|channel| AsyncChannel::from_parts(channel, self.stream.clone()))
        todo!()
    }

    pub fn host_key(&self) -> Option<(&[u8], HostKeyType)> {
        self.inner.host_key()
    }

    pub fn host_key_hash(&self, hash: HashType) -> Option<&[u8]> {
        self.inner.host_key_hash(hash)
    }

    pub async fn keepalive_send(&self) -> Result<u32, Error> {
        self.stream
            .read_and_write_with(&self.inner, || self.inner.keepalive_send())
            .await
    }

    pub async fn disconnect(
        &self,
        reason: Option<DisconnectCode>,
        description: &str,
        lang: Option<&str>,
    ) -> Result<(), Error> {
        self.stream
            .read_and_write_with(&self.inner, || {
                self.inner.disconnect(reason, description, lang)
            })
            .await
    }

    pub fn block_directions(&self) -> BlockDirections {
        self.inner.block_directions()
    }
}

//
// extension
//
impl<S> AsyncSession<S> {
    pub fn last_error(&self) -> Option<Ssh2Error> {
        Ssh2Error::last_session_error(&self.inner)
    }

    pub async fn userauth_agent_with_try_next(&self, username: &str) -> io::Result<()> {
        // let mut agent = self.agent()?;
        // agent.connect().await?;
        // agent.list_identities().await?;
        // let identities = agent.identities()?;

        // if identities.is_empty() {
        //     return Err(io::Error::new(
        //         io::ErrorKind::Other,
        //         "no identities found in the ssh agent",
        //     ));
        // }

        // for identity in identities {
        //     match agent.userauth(username, &identity).await {
        //         Ok(_) => {
        //             if self.authenticated() {
        //                 return Ok(());
        //             }
        //         }
        //         Err(_) => {
        //             continue;
        //         }
        //     }
        // }

        // Err(io::Error::new(
        //     io::ErrorKind::Other,
        //     "all identities cannot authenticated",
        // ))

        todo!()
    }
}

//
//
//
#[derive(Default, Clone)]
pub struct SessionConfiguration {
    banner: Option<String>,
    allow_sigpipe: Option<bool>,
    compress: Option<bool>,
    timeout: Option<Duration>,
    keepalive: Option<SessionKeepaliveConfiguration>,
}
impl SessionConfiguration {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn set_banner(&mut self, banner: &str) {
        self.banner = Some(banner.to_owned());
    }

    pub fn set_allow_sigpipe(&mut self, block: bool) {
        self.allow_sigpipe = Some(block);
    }

    pub fn set_compress(&mut self, compress: bool) {
        self.compress = Some(compress);
    }

    pub fn set_timeout(&mut self, timeout_ms: u32) {
        self.timeout = Some(Duration::from_millis(timeout_ms as u64));
    }

    pub fn set_keepalive(&mut self, want_reply: bool, interval: u32) {
        self.keepalive = Some(SessionKeepaliveConfiguration {
            want_reply,
            interval,
        });
    }
}

#[derive(Clone)]
struct SessionKeepaliveConfiguration {
    want_reply: bool,
    interval: u32,
}

pub(crate) fn get_session(configuration: Option<SessionConfiguration>) -> io::Result<Session> {
    let session = Session::new()?;
    session.set_blocking(false);

    if let Some(configuration) = configuration {
        if let Some(banner) = configuration.banner {
            session.set_banner(banner.as_ref())?;
        }
        if let Some(allow_sigpipe) = configuration.allow_sigpipe {
            session.set_allow_sigpipe(allow_sigpipe);
        }
        if let Some(compress) = configuration.compress {
            session.set_compress(compress);
        }
        if let Some(timeout) = configuration.timeout {
            session.set_timeout(timeout.as_millis() as u32);
        }
        if let Some(keepalive) = configuration.keepalive {
            session.set_keepalive(keepalive.want_reply, keepalive.interval);
        }
    }

    Ok(session)
}
