use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// A session-local signal that can wake a blocking connector without locking it.
/// The Hub serializes signaling and clearing with its observation ownership.
pub struct ObservationWake {
    reader: UnixStream,
    writer: UnixStream,
}

impl ObservationWake {
    pub(crate) fn new() -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Self { reader, writer })
    }

    pub(crate) fn signal(&self) -> io::Result<()> {
        (&self.writer).write_all(&[1])
    }

    /// Consume all requests made before the next poll. A request made during
    /// that poll stays readable through the following wait.
    pub(crate) fn clear(&self) -> io::Result<()> {
        (&self.reader).read_exact(&mut [0])
    }

    /// Returns true only if a source event can be read. A refresh request or
    /// timeout leaves the event stream untouched.
    pub(crate) fn wait(
        &self,
        timeout: Duration,
        source: Option<BorrowedFd<'_>>,
    ) -> io::Result<bool> {
        let mut descriptors = [
            libc::pollfd {
                fd: self.reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: source.map_or(-1, |fd| fd.as_raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let start = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            let millis = remaining.as_nanos().div_ceil(1_000_000);
            let millis = i32::try_from(millis).unwrap_or(i32::MAX);
            // SAFETY: The array is writable and both descriptors stay open
            // for the duration of the call. poll ignores the absent source (-1).
            let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, millis) };
            if ready >= 0 {
                return Ok(descriptors[0].revents == 0 && descriptors[1].revents != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
            if start.elapsed() >= timeout {
                return Ok(false);
            }
        }
    }
}
