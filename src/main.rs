extern crate mio;
#[macro_use]
extern crate clap;

struct Peer {
    socket: mio::net::TcpStream,
    buffer: [u8; 1024],
    first : usize,
    last  : usize,
    ready : mio::Ready,
    token : mio::Token,
}

struct Session {
    source     : Peer,
    target     : Peer,
}

struct SessionManager {
    sessions   : std::collections::HashMap<mio::Token, Session>,
    avail_token: std::collections::BTreeSet<mio::Token>,
    target     : std::net::SocketAddr,
}

fn main() {
    const MAX_CONN : usize = 1024;
    const MAX_EVENT: usize = MAX_CONN * 2;
    const LISTENER : mio::Token = mio::Token(MAX_EVENT);

    let target_addr;
    let listen_addr;

    let matches = clap_app!(myapp =>
        (version: "v0.1")
        (author: "lcdtyph <lcdtyph@gmail.com>")
        (@arg BIND_PORT: -b --bind +takes_value +required "Bind port")
        (@arg FORWARD:   -L +takes_value +required "Forward target")
    ).get_matches();

    if let Some(c) = matches.value_of("BIND_PORT") {
        listen_addr = (String::from("[::]:") + c).parse().unwrap();
    } else {
        unreachable!();
    }

    if let Some(c) = matches.value_of("FORWARD") {
        target_addr = c.parse().unwrap();
    } else {
        unreachable!();
    }

    let mut manager = SessionManager::new(MAX_CONN, &target_addr);
    let listener    = mio::net::TcpListener::bind(&listen_addr).unwrap();
    let poll        = mio::Poll::new().unwrap();

    poll.register(&listener, LISTENER, mio::Ready::readable(), mio::PollOpt::edge()).unwrap();

    let mut events = mio::Events::with_capacity(MAX_EVENT);
    loop {
        let n = match poll.poll(&mut events, None) {
            Ok(n) => n,
            Err(e)  => {
                println!("Error while poll: {:?}", e);
                break;
            }
        };
        drop(n);
        for event in events.iter() {
            match event.token() {
            LISTENER => {
                loop {
                    match listener.accept() {
                    Ok((connect, addr)) => {
                        println!("client {} connected", addr);
                        let (token, session) = manager.new_session(connect);

                        session.source.ready.insert(mio::Ready::readable());
                        session.source.token = token.clone();

                        session.target.ready.insert(mio::Ready::readable());
                        session.target.token = mio::Token(token.0 + MAX_CONN);

                        session.source.register(&poll, mio::PollOpt::level()).unwrap();
                        session.target.register(&poll, mio::PollOpt::level()).unwrap();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        break;
                    }
                    e => {
                        panic!("unexpected error {:?}", e);
                    }
                    }
                }
            }
            token => {
                let key = mio::Token(token.0 % MAX_CONN);
                let session = manager.get_mut(key).unwrap();
                let (this_peer, that_peer) = if token == key {
                    (&mut session.source, &mut session.target)
                } else {
                    (&mut session.target, &mut session.source)
                };
                let mut drop_session: bool = false;

                if event.readiness().is_readable() {
                    match std::io::Read::read(&mut this_peer.socket, &mut this_peer.buffer) {
                        Ok(size) => {
                            this_peer.first = 0;
                            this_peer.last = size;
                            println!("{} bytes received from {:?}", size, this_peer.socket.peer_addr());
                            if size == 0 {
                                drop_session = true;
                            } else {
                                this_peer.ready.remove(mio::Ready::readable());
                                that_peer.ready.insert(mio::Ready::writable());
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        }
                        e => {
                            println!("something error: {:?}", e);
                            drop_session = true;
                        }
                    };
                } else if event.readiness().is_writable() {
                    match std::io::Write::write(&mut this_peer.socket, &that_peer.buffer[that_peer.first..that_peer.last]) {
                        Ok(size) => {
                            that_peer.first += size;
                            if that_peer.first < that_peer.last {
                                continue;
                            }

                            this_peer.ready.remove(mio::Ready::writable());
                            that_peer.ready.insert(mio::Ready::readable());
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        }
                        e => {
                            println!("something error: {:?}", e);
                            drop_session = true;
                        }
                    }
                } else {
                    unreachable!();
                }

                if drop_session {
                    this_peer.deregister(&poll).unwrap();
                    that_peer.deregister(&poll).unwrap();
                    println!("{:?} closed socket", this_peer.socket.peer_addr());
                    drop(manager.release_session(key));
                } else {
                    this_peer.reregister(&poll, mio::PollOpt::level()).unwrap();
                    that_peer.reregister(&poll, mio::PollOpt::level()).unwrap();
                }
            }
            }
        }
    }

}

impl Peer {
    fn with_stream(socket: mio::net::TcpStream) -> Peer {
        Peer {
            socket,
            buffer: [0; 1024],
            first : 0,
            last  : 0,
            ready : mio::Ready::empty(),
            token : mio::Token(0),
        }
    }

    fn with_target(target: &std::net::SocketAddr) -> Peer {
        Peer {
            socket: mio::net::TcpStream::connect(target).unwrap(),
            buffer: [0; 1024],
            first : 0,
            last  : 0,
            ready : mio::Ready::empty(),
            token : mio::Token(0),
        }
    }

    fn register(&self, poll: &mio::Poll, opts: mio::PollOpt) -> std::io::Result<()> {
        poll.register(&self.socket, self.token.clone(), self.ready, opts)
    }

    fn reregister(&self, poll: &mio::Poll, opts: mio::PollOpt) -> std::io::Result<()> {
        poll.reregister(&self.socket, self.token.clone(), self.ready, opts)
    }

    fn deregister(&self, poll: &mio::Poll) -> std::io::Result<()> {
        poll.deregister(&self.socket)
    }
}

impl Session {
    fn new(socket: mio::net::TcpStream, target: &std::net::SocketAddr) -> Session {
        Session {
            source: Peer::with_stream(socket),
            target: Peer::with_target(target),
        }
    }
}

impl SessionManager {
    fn new(max_token: usize, target: &std::net::SocketAddr) -> SessionManager {
        let mut result = SessionManager {
            sessions   : std::collections::HashMap::new(),
            avail_token: std::collections::BTreeSet::new(),
            target     : target.clone(),
        };
        for i in 0..max_token {
            result.avail_token.insert(mio::Token(i));
        }
        result
    }

    fn new_session(&mut self, connect: mio::net::TcpStream) -> (mio::Token, &mut Session) {
        let token = *self.avail_token.iter().next().unwrap();
        self.avail_token.remove(&token);
        let session = Session::new(connect, &self.target);
        self.sessions.insert(token.clone(), session);
        let result = (token, self.sessions.get_mut(&token).unwrap());
        result
    }

    fn get_mut(&mut self, token: mio::Token) -> Option<&mut Session> {
        self.sessions.get_mut(&token)
    }

    fn release_session(&mut self, token: mio::Token) -> Session {
        self.avail_token.insert(token);
        self.sessions.remove(&token).unwrap()
    }
}

