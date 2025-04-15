use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

pub struct ReadTracker<R> {
    pub reader: R,
    pub cursor: Arc<AtomicU64>,
}

impl<R> ReadTracker<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            cursor: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl<R> std::io::Read for ReadTracker<R>
where
    R: std::io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let n = self.reader.read(buf)?;
        let n_u64: u64 = n.try_into().map_err(std::io::Error::other)?;
        self.cursor.fetch_add(n_u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl<R> std::io::BufRead for ReadTracker<R>
where
    R: std::io::BufRead,
{
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.reader.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        let amt_u64: u64 = amt.try_into().unwrap();
        self.cursor.fetch_add(amt_u64, Ordering::Relaxed);
        self.reader.consume(amt);
    }
}

impl<R> std::io::Seek for ReadTracker<R>
where
    R: std::io::Seek,
{
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let new_pos = self.reader.seek(pos)?;
        self.cursor.store(new_pos, Ordering::Relaxed);
        Ok(new_pos)
    }
}

pub struct NotSeekable<T>(pub T);

impl<T> std::io::Read for NotSeekable<T>
where
    T: std::io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        self.0.read(buf)
    }
}

impl<T> std::io::BufRead for NotSeekable<T>
where
    T: std::io::BufRead,
{
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.0.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.0.consume(amt);
    }
}

impl<T> std::io::Write for NotSeekable<T>
where
    T: std::io::Write,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl<T> std::io::Seek for NotSeekable<T> {
    fn seek(&mut self, _pos: std::io::SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::other("tried to seek NonSeekable type"))
    }
}
