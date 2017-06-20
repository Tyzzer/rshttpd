use std::{ io, fs, cmp };
use std::ops::Range;
use std::sync::Arc;
use futures::{ Stream, Poll, Async };
use futures::sync::BiLock;
use tokio_io::io::Window;
use tokio_core::net::TcpStream;
use tokio_core::reactor::Handle;
use hyper;
use ::sendfile::SendFileFut;
use ::error;


pub const CHUNK_BUFF_LENGTH: usize = 1 << 16;

pub struct File {
    fd: fs::File,
    pub len: u64
}

impl File {
    #[inline]
    pub fn new(fd: fs::File, _handle: Handle, len: u64) -> io::Result<Self> {
        Ok(File { fd, len })
    }

    pub fn read(&self, range: Range<u64>) -> io::Result<ReadChunkFut> {
        let fd = self.fd.try_clone()?;
        let buf = vec![0; cmp::min(CHUNK_BUFF_LENGTH, self.len as _)];

        Ok(ReadChunkFut { fd, range, buf })
    }

    pub fn sendfile(&self, range: Range<u64>, socket: Arc<BiLock<TcpStream>>) -> io::Result<SendFileFut> {
        let fd = self.fd.try_clone()?;
        Ok(SendFileFut {
            socket, fd,
            offset: range.start as _,
            count: (range.end - range.start) as _
        })
    }
}

pub struct ReadChunkFut {
    fd: fs::File,
    range: Range<u64>,
    buf: Vec<u8>
}

impl Stream for ReadChunkFut {
    type Item = hyper::Result<hyper::Chunk>;
    type Error = error::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        #[cfg(unix)] use std::os::unix::fs::FileExt;
        #[cfg(windows)] use std::os::windows::fs::FileExt;

        let want_len = cmp::min((self.range.end - self.range.start) as _, self.buf.len());

        if want_len > 0 {
            let mut window = Window::new(&mut self.buf[..]);
            window.set_end(want_len);

            #[cfg(unix)]
            let read_len = self.fd.read_at(window.as_mut(), self.range.start)?;

            #[cfg(windows)]
            let read_len = self.fd.seek_read(window.as_mut(), self.range.start)?;

            self.range.start += read_len as _;
            window.set_end(read_len);
            let chunk = Vec::from(window.as_ref());
            let chunk = hyper::Chunk::from(chunk);
            Ok(Async::Ready(Some(Ok(chunk))))
        } else {
            Ok(Async::Ready(None))
        }
    }
}


#[cfg(test)]
mod test {
    extern crate tempdir;

    use std::fs;
    use std::io::Write;
    use futures::Stream;
    use tokio_core::reactor::Core;
    use self::tempdir::TempDir;
    use super::*;

    #[test]
    fn test_file() {
        let tmp = TempDir::new("webdir_test_file").unwrap();

        {
            fs::File::create(tmp.path().join("test")).unwrap()
                .write_all(&[42; 1024]).unwrap();
        }

        let mut core = Core::new().unwrap();
        let handle = core.handle();
        let fd = fs::File::open(tmp.path().join("test")).unwrap();
        let len = fd.metadata().unwrap().len();

        let fd = File::new(fd, handle, len as _).unwrap();
        let fut = fd.read(32..1021).unwrap()
            .map(|chunk| chunk.unwrap().to_vec())
            .concat2();

        let output = core.run(fut).unwrap();

        assert_eq!(output, &[42; 989][..]);
    }
}
