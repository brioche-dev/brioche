use std::{
    pin::Pin,
    sync::OnceLock,
    task::{ready, Poll},
};

pin_project_lite::pin_project! {
    pub struct ZstdSeekableEncoder<W> {
        #[pin]
        writer: W,
        cstream: zstd_seekable::SeekableCStream,
        buffer: Vec<u8>,
        unwritten_start: usize,
        unwritten_end: usize,
        did_write_end: bool,
    }
}

impl<W> ZstdSeekableEncoder<W> {
    pub fn new(writer: W, level: usize, frame_size: usize) -> Result<Self, zstd_seekable::Error> {
        let cstream = zstd_seekable::SeekableCStream::new(level, frame_size)?;
        Ok(Self {
            writer,
            cstream,
            buffer: vec![0; zstd_seekable::CStream::out_size()],
            unwritten_start: 0,
            unwritten_end: 0,
            did_write_end: false,
        })
    }

    fn flush_pending(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>>
    where
        W: tokio::io::AsyncWrite,
    {
        let mut this = self.project();

        loop {
            let pending = &this.buffer[*this.unwritten_start..*this.unwritten_end];

            if pending.is_empty() {
                *this.unwritten_start = 0;
                *this.unwritten_end = 0;

                return Poll::Ready(Ok(()));
            }

            let written = ready!(this.writer.as_mut().poll_write(cx, pending))?;
            if written == 0 {
                return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
            }

            *this.unwritten_start += written;
        }
    }
}

impl<W> tokio::io::AsyncWrite for ZstdSeekableEncoder<W>
where
    W: tokio::io::AsyncWrite,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        static ZSTD_CSTREAM_IN_SIZE: OnceLock<usize> = OnceLock::new();
        let in_size = *ZSTD_CSTREAM_IN_SIZE.get_or_init(zstd_seekable::CStream::in_size);

        ready!(self.as_mut().flush_pending(cx))?;

        let this = self.project();

        let read_len = buf.len().min(in_size);

        let (out_pos, in_pos) = this
            .cstream
            .compress(this.buffer, &buf[..read_len])
            .map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::Other, format!("zstd error: {error}"))
            })?;

        *this.unwritten_end = out_pos;

        Poll::Ready(Ok(in_pos))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.flush_pending(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        ready!(self.as_mut().flush_pending(cx))?;

        let this = self.as_mut().project();

        if !*this.did_write_end {
            let out_pos = this.cstream.end_stream(this.buffer).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::Other, format!("zstd error: {error}"))
            })?;

            *this.unwritten_end = out_pos;
            *this.did_write_end = true;

            ready!(self.as_mut().flush_pending(cx))?;
        }

        let this = self.project();

        // Take the buffer to drop it. This will effectively close the
        // writer, since writes won't have anywhere to go to
        std::mem::take(this.buffer);

        this.writer.poll_shutdown(cx)
    }
}

pub struct ZstdSeekableDecoder<'a, R> {
    seekable: zstd_seekable::Seekable<'a, R>,
    cursor: u64,
    decompressed_length: Option<u64>,
}

impl<'a, R> ZstdSeekableDecoder<'a, R> {
    pub fn new(reader: Box<R>) -> Result<Self, zstd_seekable::Error>
    where
        R: std::io::Read + std::io::Seek,
    {
        let seekable = zstd_seekable::Seekable::init(reader)?;
        Ok(Self {
            seekable,
            cursor: 0,
            decompressed_length: None,
        })
    }
}

impl<'a, R> std::io::Read for ZstdSeekableDecoder<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let result = self
            .seekable
            .decompress(buf, self.cursor)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        let result_u64: u64 = result.try_into().expect("zstd cursor out of range");
        self.cursor = self
            .cursor
            .checked_add(result_u64)
            .expect("zstd cursor out of range");

        Ok(result)
    }
}

impl<'a, R> std::io::Seek for ZstdSeekableDecoder<'a, R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match pos {
            std::io::SeekFrom::Start(offset) => {
                self.cursor = offset;
                Ok(self.cursor)
            }
            std::io::SeekFrom::End(offset) => {
                let decompressed_length = match self.decompressed_length {
                    Some(decompressed_length) => decompressed_length,
                    None => {
                        let num_frames = dbg!(self.seekable.get_num_frames());

                        let last_frame_index = num_frames.checked_sub(1);
                        let decompressed_length = last_frame_index
                            .map(|last_frame_index| {
                                let last_frame_offset = dbg!(self
                                    .seekable
                                    .get_frame_decompressed_offset(last_frame_index));
                                let last_frame_size = dbg!(self
                                    .seekable
                                    .get_frame_decompressed_size(last_frame_index));
                                let last_frame_size: u64 =
                                    dbg!(last_frame_size.try_into().map_err(|_| {
                                        std::io::Error::other("invalid zstd frame size")
                                    })?);

                                let decompressed_length = last_frame_offset
                                    .checked_add(last_frame_size)
                                    .ok_or_else(|| {
                                        std::io::Error::other("invalid zstd decompressed length")
                                    })?;

                                std::io::Result::Ok(decompressed_length)
                            })
                            .transpose()?;
                        let decompressed_length = decompressed_length.unwrap_or(0);

                        self.decompressed_length = Some(decompressed_length);
                        decompressed_length
                    }
                };

                let cursor: u64 = decompressed_length
                    .checked_add_signed(offset)
                    .ok_or_else(|| std::io::Error::other("tried to seek out of range"))?;
                self.cursor = dbg!(cursor);
                Ok(self.cursor)
            }
            std::io::SeekFrom::Current(offset) => {
                self.cursor = self
                    .cursor
                    .checked_add_signed(offset)
                    .expect("tried to seek out-of-range");
                Ok(self.cursor)
            }
        }
    }
}
