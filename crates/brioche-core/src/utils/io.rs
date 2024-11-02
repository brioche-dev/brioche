pub struct ReadTracker<R> {
    pub reader: R,
    pub cursor: u64,
}

impl<R> ReadTracker<R> {
    pub fn new(reader: R) -> Self {
        Self { reader, cursor: 0 }
    }
}

impl<R> std::io::Read for ReadTracker<R>
where
    R: std::io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let n = self.reader.read(buf)?;
        let n_u64: u64 = n.try_into().map_err(std::io::Error::other)?;
        self.cursor += n_u64;
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
        self.cursor += amt_u64;
        self.reader.consume(amt);
    }
}

impl<R> std::io::Seek for ReadTracker<R>
where
    R: std::io::Seek,
{
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let new_pos = self.reader.seek(pos)?;
        self.cursor = new_pos;
        Ok(new_pos)
    }
}
