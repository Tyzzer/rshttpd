extern crate bytes;

use std::{ io, fs, mem };
use std::sync::Arc;
use std::os::unix::io::AsRawFd;
use futures::{ Poll, Stream, Async };
use futures::sync::{ BiLock, BiLockAcquired };
use mio::net::TcpStream as MioTcpStream;
use tokio::net::TcpStream;
use tokio::reactor::PollEvented2;
use tokio_io::{ AsyncRead, AsyncWrite };
use tokio_fs::File as TokioFile;
use nix::{ self, errno };
use nix::libc::off_t;
use self::bytes::buf::{ Buf, BufMut };
use ::error;


pub struct BiTcpStream(pub BiLockAcquired<TcpStream>);

impl io::Read for BiTcpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl io::Write for BiTcpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl AsyncRead for BiTcpStream {
    unsafe fn prepare_uninitialized_buffer(&self, buf: &mut [u8]) -> bool {
        AsyncRead::prepare_uninitialized_buffer(&*self.0, buf)
    }

    fn read_buf<B: BufMut>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        AsyncRead::read_buf(&mut *self.0, buf)
    }
}

impl AsyncWrite for BiTcpStream {
    fn shutdown(&mut self) -> Poll<(), io::Error> {
        AsyncWrite::shutdown(&mut *self.0)
    }

    fn write_buf<B: Buf>(&mut self, buf: &mut B) -> Poll<usize, io::Error> {
        AsyncWrite::write_buf(&mut *self.0, buf)
    }
}


pub struct SendFileFut {
    socket: Arc<BiLock<TcpStream>>,
    fd: fs::File,
    offset: off_t,
    end: usize
}

impl Stream for SendFileFut {
    type Item = usize;
    type Error = error::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        use nix::sys::sendfile::sendfile;

        #[cfg(any(apple, freebsdlike))]
        use self::bsd::sendfile;

        unsafe fn as_poll_evented(t: &mut TcpStream) -> &mut PollEvented2<MioTcpStream> {
            struct PubTcpStream {
                pub io: PollEvented2<MioTcpStream>
            }

            &mut mem::transmute::<_, &mut PubTcpStream>(t).io
        }


        let count = match self.end.checked_sub(self.offset as usize) {
            Some(0) | None => return Ok(Async::Ready(None)),
            Some(count) => count
        };

        let mut socket = match self.socket.poll_lock() {
            Async::Ready(socket) => socket,
            Async::NotReady => return Ok(Async::NotReady)
        };

        let socket = unsafe { as_poll_evented(&mut socket) };

        if let Async::NotReady = socket.poll_write_ready()? {
            return Ok(Async::NotReady)
        }

        match sendfile(socket.get_ref().as_raw_fd(), self.fd.as_raw_fd(), Some(&mut self.offset), count) {
            Ok(len) => {
                Ok(Async::Ready(Some(len)))
            },
            Err(ref err) if &nix::Error::Sys(errno::Errno::EAGAIN) == err => {
                // TODO https://github.com/tokio-rs/tokio-core/issues/196
                // socket.need_write();

                socket.clear_write_ready()?;
                Ok(Async::NotReady)
            },
            Err(err) => Err(err.into())
        }
    }
}


#[cfg(any(apple, freebsdlike))]
mod bsd {
    use std::ptr;
    use std::os::unix::io::RawFd;
    use nix::libc::{ off_t, sendfile as libc_sendfile };
    use nix;

    pub fn sendfile(out_fd: RawFd, in_fd: RawFd, offset: Option<&mut off_t>, count: usize) -> nix::Result<usize> {
        let off =
            if let Some(&mut off) = offset { off }
            else { 0 };
        let mut len = count as _;

        #[cfg(apple)]
        let ret = unsafe { libc_sendfile(in_fd, out_fd, off, &mut len, ptr::null_mut(), 0) };

        #[cfg(freebsdlike)]
        let ret = unsafe { libc_sendfile(in_fd, out_fd, off, count, ptr::null_mut(), &mut len, 0) };

        if let Some(offset) = offset {
            *offset += len;
        }

        match ret {
            0 => Ok(len as usize),
            _ => Err(nix::Error::last())
        }
    }
}
