use std::collections::{BTreeMap, VecDeque};

use bstr::{BString, ByteSlice as _};

/// A type that buffers streams of data similar to a terminal. Each stream's
/// output is buffered separately, keyed with an arbitrary stream type (`K`).
/// When adding data to the buffer, complete lines are written to the output
/// and the remainder is stored in a partial buffer, until it's either flushed
/// explicitly or until more data is added to form a complete line.
///
/// The buffer can also limit the total number of bytes stored to some upper
/// limit. The oldest data is removed from the buffer first.
pub struct OutputBuffer<K>
where
    K: Clone + Ord,
{
    total_bytes: usize,
    max_bytes: Option<usize>,
    contents: VecDeque<(K, BString)>,
    partial_append: BTreeMap<K, BString>,
    partial_prepend: BTreeMap<K, BString>,
}

impl<K> OutputBuffer<K>
where
    K: Clone + Ord,
{
    pub fn with_max_capacity(max_bytes: usize) -> Self {
        Self {
            total_bytes: 0,
            max_bytes: Some(max_bytes),
            contents: VecDeque::new(),
            partial_append: BTreeMap::new(),
            partial_prepend: BTreeMap::new(),
        }
    }

    pub fn with_unlimited_capacity() -> Self {
        Self {
            total_bytes: 0,
            max_bytes: None,
            contents: VecDeque::new(),
            partial_append: BTreeMap::new(),
            partial_prepend: BTreeMap::new(),
        }
    }

    pub fn append(&mut self, stream: K, content: impl AsRef<[u8]>) {
        let content = content.as_ref();

        // Truncate content so that it fits within `max_bytes`
        let content = match self.max_bytes {
            Some(max_bytes) => {
                let content_start = content.len().saturating_sub(max_bytes);
                &content[content_start..]
            }
            None => content,
        };

        // Break the content into the part containing complete lines, and the
        // part that's made up of only partial lines
        let (complete_content, partial_content) = match content.rsplit_once_str("\n") {
            Some((complete, partial)) => (Some(complete), partial),
            None => (None, content),
        };

        // Drop old content until we have enough free space to add the new content
        let new_total_bytes = self.total_bytes.saturating_add(content.len());
        let mut drop_bytes = self
            .max_bytes
            .map_or(0, |max_bytes| new_total_bytes.saturating_sub(max_bytes));
        while drop_bytes > 0 {
            // Get the oldest content
            let oldest_content = self
                .partial_prepend
                .first_entry()
                .map(|entry| entry.into_mut())
                .or_else(|| self.contents.get_mut(0).map(|(_, content)| content))
                .or_else(|| {
                    self.partial_append
                        .first_entry()
                        .map(|entry| entry.into_mut())
                });
            let Some(oldest_content) = oldest_content else {
                break;
            };

            if oldest_content.len() > drop_bytes {
                // If the oldest content is longer than the total number of
                // bytes need to drop, then remove the bytes at the start, then
                // we're done
                oldest_content.drain(0..drop_bytes);
                break;
            } else {
                // Otherwise, remove the content and continue
                let (_, removed_content) = self
                    .partial_prepend
                    .pop_first()
                    .or_else(|| self.contents.pop_front())
                    .or_else(|| self.partial_append.pop_first())
                    .unwrap();
                drop_bytes -= removed_content.len();
            }
        }

        if let Some(complete_content) = complete_content {
            let prior_partial = self.partial_append.remove(&stream);
            let prior_content = self
                .contents
                .back_mut()
                .and_then(|(content_stream, content)| {
                    if *content_stream == stream {
                        Some(content)
                    } else {
                        None
                    }
                });

            if let Some(prior_content) = prior_content {
                // If the most recent content is from the same stream, then
                // just append the partial content and new content to the end

                if let Some(prior_partial) = prior_partial {
                    prior_content.extend_from_slice(&prior_partial);
                }
                prior_content.extend_from_slice(complete_content);
                prior_content.push(b'\n');
            } else {
                // Otherwise, add a new content entry

                let mut bytes = bstr::BString::default();
                if let Some(prior_partial) = prior_partial {
                    bytes.extend_from_slice(&prior_partial);
                }
                bytes.extend_from_slice(complete_content);
                bytes.push(b'\n');

                self.contents.push_back((stream.clone(), bytes));
            }
        }

        if !partial_content.is_empty() {
            self.partial_append
                .entry(stream)
                .or_default()
                .extend_from_slice(partial_content);
        }

        self.total_bytes = match self.max_bytes {
            Some(max_bytes) => std::cmp::min(new_total_bytes, max_bytes),
            None => new_total_bytes,
        };
    }

    pub fn prepend(&mut self, stream: K, content: impl AsRef<[u8]>) {
        let content = content.as_ref();

        // Break the content into the part containing complete lines, and the
        // part that's made up of only partial lines. We do this before
        // truncation to properly detect when to flush partial lines
        let (partial_content, complete_content) = match content.split_once_str("\n") {
            Some((partial, complete)) => (partial, Some(complete)),
            None => (content, None),
        };

        let separator = complete_content.map(|_| &b"\n"[..]);

        // Truncate content so that it fits within the remaining bytes.
        // Since the content we're prepending is by definition the oldest
        // content, we don't need to drop or truncate any other content
        let (partial_content, complete_content, separator) = match self.max_bytes {
            Some(max_bytes) => {
                // If we have any complete content, truncate it so it fits
                // within the remaining space
                let remaining_bytes = max_bytes.saturating_sub(self.total_bytes);
                let complete_content = complete_content.map(|complete| {
                    let content_start = complete.len().saturating_sub(remaining_bytes);
                    &complete[content_start..]
                });
                let complete_content_length = complete_content.map_or(0, |complete| complete.len());

                // If we have a separator (newline) to add, truncate it
                // so it fits within the remaining space
                let remaining_bytes = remaining_bytes.saturating_sub(complete_content_length);
                let separator = separator.map(|separator| {
                    let separator_start = separator.len().saturating_sub(remaining_bytes);
                    &separator[separator_start..]
                });
                let separator_length = separator.map_or(0, |separator| separator.len());

                // Truncate the partial content to fit within whatever
                // space we have left over
                let remaining_bytes = remaining_bytes.saturating_sub(separator_length);
                let partial_content_start = partial_content.len().saturating_sub(remaining_bytes);
                let partial_content = &partial_content[partial_content_start..];

                (partial_content, complete_content, separator)
            }
            None => (partial_content, complete_content, separator),
        };

        self.total_bytes = self
            .total_bytes
            .saturating_add(partial_content.len())
            .saturating_add(complete_content.map_or(0, |content| content.len()))
            .saturating_add(separator.map_or(0, |separator| separator.len()));

        if let Some(complete_content) = complete_content {
            let prior_partial = self.partial_prepend.remove(&stream);
            let prior_content = self
                .contents
                .front_mut()
                .and_then(|(content_stream, content)| {
                    if *content_stream == stream {
                        Some(content)
                    } else {
                        None
                    }
                });

            if let Some(content) = prior_content {
                // If the most recent content is from the same stream, then
                // prepend the partial content and new content to the beginning

                let tail = std::mem::take(content);

                content.extend_from_slice(complete_content);
                if let Some(prior_partial) = prior_partial {
                    content.extend_from_slice(&prior_partial);
                }
                content.extend_from_slice(&tail);
            } else {
                // Otherwise, add a new content entry

                let mut content = bstr::BString::default();

                content.extend_from_slice(complete_content);
                if let Some(prior_partial) = prior_partial {
                    content.extend_from_slice(&prior_partial);
                }

                self.contents.push_front((stream.clone(), content));
            }
        }

        match self.partial_prepend.entry(stream) {
            std::collections::btree_map::Entry::Occupied(entry) => {
                // If there's already partial content for the stream,
                // add the new partial content to the beginning

                let content = entry.into_mut();

                if !partial_content.is_empty() {
                    let tail = std::mem::take(content);

                    content.extend_from_slice(partial_content);
                    content.extend_from_slice(&tail);
                }

                if let Some(separator) = separator {
                    content.extend_from_slice(separator);
                }
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                // Otherwise, build a new partial content entry

                let mut content = bstr::BString::default();

                content.extend_from_slice(partial_content);
                if let Some(separator) = separator {
                    content.extend_from_slice(separator);
                }

                // Only add the partial content if we have something to add
                if !content.is_empty() {
                    entry.insert(content);
                }
            }
        }
    }

    pub fn flush_stream(&mut self, stream: K) {
        // If there's partial content to prepend, then add it
        // to the beginning of the stream
        if let Some(mut content) = self.partial_prepend.remove(&stream) {
            let prior_content = self
                .contents
                .front_mut()
                .and_then(|(content_stream, content)| {
                    if *content_stream == stream {
                        Some(content)
                    } else {
                        None
                    }
                });

            if let Some(prior_content) = prior_content {
                // If the first content entry is from the same stream, add the
                // partial content to the beginning of it
                let tail = std::mem::take(prior_content);
                content.extend_from_slice(&tail);
                *prior_content = content;
            } else {
                // Otherwise, prepend a new content entry
                self.contents.push_front((stream.clone(), content));
            }
        }

        // If there's partial content to append, then add it
        // to the end of the stream
        if let Some(tail) = self.partial_append.remove(&stream) {
            let prior_content = self
                .contents
                .back_mut()
                .and_then(|(content_stream, content)| {
                    if *content_stream == stream {
                        Some(content)
                    } else {
                        None
                    }
                });

            if let Some(prior_content) = prior_content {
                // If the last content entry is from the same stream, add the
                // partial content to the end of it
                prior_content.extend_from_slice(&tail);
            } else {
                // Otherwise, append a new content entry
                self.contents.push_back((stream, tail));
            }
        }
    }

    pub fn contents(&self) -> impl DoubleEndedIterator<Item = (&K, &bstr::BStr)> {
        self.contents
            .iter()
            .map(|(stream, content)| (stream, bstr::BStr::new(content)))
    }

    pub fn pop_front(&mut self) -> Option<(K, bstr::BString)> {
        let content = self.contents.pop_front();
        if let Some((_, content)) = &content {
            self.total_bytes = self.total_bytes.saturating_sub(content.len());
        }

        content
    }

    pub fn pop_back(&mut self) -> Option<(K, bstr::BString)> {
        let content = self.contents.pop_back();
        if let Some((_, content)) = &content {
            self.total_bytes = self.total_bytes.saturating_sub(content.len());
        }

        content
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::reporter::job::ProcessStream::{self, Stderr, Stdout};

    type JobOutputBuffer = super::OutputBuffer<JobOutputStream>;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    struct JobOutputStream {
        job_id: usize,
        stream: ProcessStream,
    }

    fn job_stream(job_id: usize, stream: ProcessStream) -> JobOutputStream {
        JobOutputStream { job_id, stream }
    }

    #[test]
    fn test_output_buffer_basic() {
        let mut output = JobOutputBuffer::with_unlimited_capacity();
        output.append(job_stream(1, Stdout), "a\nb\nc");

        assert_eq!(output.total_bytes, 5);
        assert_eq!(output.contents, [(job_stream(1, Stdout), "a\nb\n".into())],);
        assert_eq!(
            output.partial_append,
            BTreeMap::from_iter([(job_stream(1, Stdout), "c".into())])
        );
    }

    #[test]
    fn test_output_buffer_interleaved() {
        let mut output = JobOutputBuffer::with_unlimited_capacity();

        output.append(job_stream(1, Stdout), "a\nb\nc");
        output.append(job_stream(1, Stderr), "d\ne\nf");
        output.append(job_stream(2, Stdout), "g\nh\ni");
        output.append(job_stream(2, Stdout), "j\nk\nl");
        output.append(job_stream(2, Stderr), "m\nn\no");
        output.append(job_stream(2, Stderr), "p\nq\nr");
        output.append(job_stream(1, Stdout), "s\nt\nu");
        output.append(job_stream(1, Stderr), "v\nw\nx");

        assert_eq!(output.total_bytes, 40);
        assert_eq!(
            output.contents,
            [
                (job_stream(1, Stdout), "a\nb\n".into()),
                (job_stream(1, Stderr), "d\ne\n".into()),
                (job_stream(2, Stdout), "g\nh\nij\nk\n".into()),
                (job_stream(2, Stderr), "m\nn\nop\nq\n".into()),
                (job_stream(1, Stdout), "cs\nt\n".into()),
                (job_stream(1, Stderr), "fv\nw\n".into()),
            ]
        );
        assert_eq!(
            output.partial_append,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "u".into()),
                (job_stream(1, Stderr), "x".into()),
                (job_stream(2, Stdout), "l".into()),
                (job_stream(2, Stderr), "r".into()),
            ])
        );
    }

    #[test]
    fn test_output_buffer_drop_oldest() {
        let mut output = JobOutputBuffer::with_max_capacity(10);

        output.append(job_stream(1, Stdout), "a\n");

        assert_eq!(output.total_bytes, 2);
        assert_eq!(output.contents, [(job_stream(1, Stdout), "a\n".into())]);

        output.append(job_stream(2, Stdout), "bcdefghij\n");

        assert_eq!(output.total_bytes, 10);
        assert_eq!(
            output.contents,
            [(job_stream(2, Stdout), "bcdefghij\n".into())]
        );
    }

    #[test]
    fn test_output_buffer_drop_partial_oldest() {
        let mut output = JobOutputBuffer::with_max_capacity(5);

        output.append(job_stream(1, Stdout), "a");
        output.append(job_stream(2, Stdout), "b");
        output.append(job_stream(3, Stdout), "c");
        output.append(job_stream(4, Stdout), "d");
        output.append(job_stream(5, Stdout), "e");

        assert_eq!(output.total_bytes, 5);
        assert!(output.contents.is_empty());
        assert_eq!(
            output.partial_append,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "a".into()),
                (job_stream(2, Stdout), "b".into()),
                (job_stream(3, Stdout), "c".into()),
                (job_stream(4, Stdout), "d".into()),
                (job_stream(5, Stdout), "e".into()),
            ])
        );

        output.append(job_stream(6, Stdout), "f");

        assert_eq!(output.total_bytes, 5);
        assert!(output.contents.is_empty());
        assert_eq!(
            output.partial_append,
            BTreeMap::from_iter([
                (job_stream(2, Stdout), "b".into()),
                (job_stream(3, Stdout), "c".into()),
                (job_stream(4, Stdout), "d".into()),
                (job_stream(5, Stdout), "e".into()),
                (job_stream(6, Stdout), "f".into()),
            ]),
        );

        output.append(job_stream(7, Stdout), "g");

        assert_eq!(output.total_bytes, 5);
        assert!(output.contents.is_empty());
        assert_eq!(
            output.partial_append,
            BTreeMap::from_iter([
                (job_stream(3, Stdout), "c".into()),
                (job_stream(4, Stdout), "d".into()),
                (job_stream(5, Stdout), "e".into()),
                (job_stream(6, Stdout), "f".into()),
                (job_stream(7, Stdout), "g".into()),
            ]),
        );
    }

    #[test]
    fn test_output_buffer_truncate_oldest() {
        let mut output = JobOutputBuffer::with_max_capacity(10);

        output.append(job_stream(1, Stdout), "abcdefghi\n");

        assert_eq!(output.total_bytes, 10);
        assert_eq!(
            output.contents,
            [(job_stream(1, Stdout), "abcdefghi\n".into()),]
        );

        output.append(job_stream(2, Stdout), "jk\n");

        assert_eq!(output.total_bytes, 10);
        assert_eq!(
            output.contents,
            [
                (job_stream(1, Stdout), "defghi\n".into()),
                (job_stream(2, Stdout), "jk\n".into()),
            ]
        );
    }

    #[test]
    fn test_output_buffer_prepend() {
        let mut output = JobOutputBuffer::with_unlimited_capacity();

        output.prepend(job_stream(1, Stdout), "x\ny\nz");

        assert_eq!(output.total_bytes, 5);
        assert_eq!(output.contents, [(job_stream(1, Stdout), "y\nz".into()),]);
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([(job_stream(1, Stdout), "x\n".into())])
        );

        output.prepend(job_stream(2, Stderr), "\nX\nY\nZ");

        assert_eq!(output.total_bytes, 11);
        assert_eq!(
            output.contents,
            [
                (job_stream(2, Stderr), "X\nY\nZ".into()),
                (job_stream(1, Stdout), "y\nz".into()),
            ]
        );
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "x\n".into()),
                (job_stream(2, Stderr), "\n".into()),
            ])
        );

        output.prepend(job_stream(1, Stdout), "w");
        assert_eq!(output.total_bytes, 12);
        assert_eq!(
            output.contents,
            [
                (job_stream(2, Stderr), "X\nY\nZ".into()),
                (job_stream(1, Stdout), "y\nz".into()),
            ]
        );
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "wx\n".into()),
                (job_stream(2, Stderr), "\n".into()),
            ])
        );

        output.prepend(job_stream(1, Stdout), "t\nu\nv");

        assert_eq!(output.total_bytes, 17);
        assert_eq!(
            output.contents,
            [
                (job_stream(1, Stdout), "u\nvwx\n".into()),
                (job_stream(2, Stderr), "X\nY\nZ".into()),
                (job_stream(1, Stdout), "y\nz".into()),
            ]
        );
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "t\n".into()),
                (job_stream(2, Stderr), "\n".into()),
            ])
        );

        output.prepend(job_stream(1, Stdout), "\nqrs\n");

        assert_eq!(output.total_bytes, 22);
        assert_eq!(
            output.contents,
            [
                (job_stream(1, Stdout), "qrs\nt\nu\nvwx\n".into()),
                (job_stream(2, Stderr), "X\nY\nZ".into()),
                (job_stream(1, Stdout), "y\nz".into()),
            ]
        );
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "\n".into()),
                (job_stream(2, Stderr), "\n".into()),
            ])
        );
    }

    #[test]
    fn test_output_buffer_prepend_truncate() {
        let mut output = JobOutputBuffer::with_max_capacity(12);

        output.prepend(job_stream(1, Stdout), "x\ny\nz");

        assert_eq!(output.total_bytes, 5);
        assert_eq!(output.contents, [(job_stream(1, Stdout), "y\nz".into())]);
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([(job_stream(1, Stdout), "x\n".into()),]),
        );

        output.prepend(job_stream(2, Stderr), "X\nY\nZ");

        assert_eq!(output.total_bytes, 10);
        assert_eq!(
            output.contents,
            [
                (job_stream(2, Stderr), "Y\nZ".into()),
                (job_stream(1, Stdout), "y\nz".into()),
            ]
        );
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "x\n".into()),
                (job_stream(2, Stderr), "X\n".into()),
            ]),
        );

        output.prepend(job_stream(2, Stderr), "T\nU\nVW");
        assert_eq!(output.total_bytes, 12);
        assert_eq!(
            output.contents,
            [
                (job_stream(2, Stderr), "VWX\nY\nZ".into()),
                (job_stream(1, Stdout), "y\nz".into()),
            ]
        );
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([(job_stream(1, Stdout), "x\n".into()),]),
        );
    }

    #[test]
    fn test_output_buffer_flush() {
        let mut output = JobOutputBuffer::with_unlimited_capacity();

        output.append(job_stream(1, Stdout), "a\nb\nc");

        assert_eq!(output.total_bytes, 5);
        assert_eq!(output.contents, [(job_stream(1, Stdout), "a\nb\n".into())],);
        assert_eq!(
            output.partial_append,
            BTreeMap::from_iter([(job_stream(1, Stdout), "c".into())]),
        );
        assert!(output.partial_prepend.is_empty());

        output.flush_stream(job_stream(1, Stdout));

        assert_eq!(output.total_bytes, 5);
        assert_eq!(output.contents, [(job_stream(1, Stdout), "a\nb\nc".into())]);
        assert!(output.partial_append.is_empty());
        assert!(output.partial_prepend.is_empty());

        output.append(job_stream(1, Stdout), "d\ne\n");

        assert_eq!(
            output.contents,
            [(job_stream(1, Stdout), "a\nb\ncd\ne\n".into())]
        );
        assert!(output.partial_append.is_empty());
        assert!(output.partial_prepend.is_empty());

        output.prepend(job_stream(1, Stdout), "X\nY\nZ");

        assert_eq!(
            output.contents,
            [(job_stream(1, Stdout), "Y\nZa\nb\ncd\ne\n".into())]
        );
        assert!(output.partial_append.is_empty());
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([(job_stream(1, Stdout), "X\n".into())])
        );

        output.flush_stream(job_stream(1, Stdout));

        assert_eq!(
            output.contents,
            [(job_stream(1, Stdout), "X\nY\nZa\nb\ncd\ne\n".into())]
        );
        assert!(output.partial_append.is_empty());
        assert!(output.partial_prepend.is_empty());
    }

    #[test]
    fn test_output_buffer_pop_front() {
        let mut output = JobOutputBuffer::with_unlimited_capacity();

        output.append(job_stream(1, Stdout), "a\nb\nc");
        output.append(job_stream(2, Stderr), "d\ne\nf");

        assert_eq!(
            output.pop_front(),
            Some((job_stream(1, Stdout), "a\nb\n".into()))
        );

        assert_eq!(output.total_bytes, 6);
        assert_eq!(output.contents, [(job_stream(2, Stderr), "d\ne\n".into())]);
        assert_eq!(
            output.partial_append,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "c".into()),
                (job_stream(2, Stderr), "f".into())
            ])
        );

        assert_eq!(
            output.pop_front(),
            Some((job_stream(2, Stderr), "d\ne\n".into()))
        );

        assert_eq!(output.total_bytes, 2);
        assert!(output.contents.is_empty());
        assert_eq!(
            output.partial_append,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "c".into()),
                (job_stream(2, Stderr), "f".into())
            ])
        );

        assert_eq!(output.pop_front(), None);

        assert_eq!(output.total_bytes, 2);
        assert!(output.contents.is_empty());
        assert_eq!(
            output.partial_append,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "c".into()),
                (job_stream(2, Stderr), "f".into())
            ])
        );
    }

    #[test]
    fn test_output_buffer_pop_back() {
        let mut output = JobOutputBuffer::with_unlimited_capacity();

        output.prepend(job_stream(1, Stdout), "a\nb\nc");
        output.prepend(job_stream(2, Stderr), "d\ne\nf");

        assert_eq!(
            output.pop_back(),
            Some((job_stream(1, Stdout), "b\nc".into()))
        );

        assert_eq!(output.total_bytes, 7);
        assert_eq!(output.contents, [(job_stream(2, Stderr), "e\nf".into())]);
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "a\n".into()),
                (job_stream(2, Stderr), "d\n".into())
            ])
        );

        assert_eq!(
            output.pop_back(),
            Some((job_stream(2, Stderr), "e\nf".into()))
        );

        assert_eq!(output.total_bytes, 4);
        assert!(output.contents.is_empty());
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "a\n".into()),
                (job_stream(2, Stderr), "d\n".into())
            ])
        );

        assert_eq!(output.pop_back(), None);

        assert_eq!(output.total_bytes, 4);
        assert!(output.contents.is_empty());
        assert_eq!(
            output.partial_prepend,
            BTreeMap::from_iter([
                (job_stream(1, Stdout), "a\n".into()),
                (job_stream(2, Stderr), "d\n".into())
            ])
        );
    }
}
