use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
};

use bstr::{BString, ByteSlice as _};

pub struct OutputBuffer<K>
where
    K: Clone + Eq + Hash,
{
    total_bytes: usize,
    max_bytes: Option<usize>,
    contents: VecDeque<(K, BString)>,
    partial_contents: HashMap<K, BString>,
}

impl<K> OutputBuffer<K>
where
    K: Clone + Eq + Hash,
{
    pub fn with_max_capacity(max_bytes: usize) -> Self {
        Self {
            total_bytes: 0,
            max_bytes: Some(max_bytes),
            contents: VecDeque::new(),
            partial_contents: HashMap::new(),
        }
    }

    pub fn with_unlimited_capacity() -> Self {
        Self {
            total_bytes: 0,
            max_bytes: None,
            contents: VecDeque::new(),
            partial_contents: HashMap::new(),
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
            Some((complete, pending)) => (Some(complete), pending),
            None => (None, content),
        };

        // Drop old content until we have enough free space to add the new content
        let new_total_bytes = self.total_bytes.saturating_add(content.len());
        let mut drop_bytes = self
            .max_bytes
            .map(|max_bytes| new_total_bytes.saturating_sub(max_bytes))
            .unwrap_or(0);
        while drop_bytes > 0 {
            // Get the oldest content
            let oldest_content = self
                .contents
                .get_mut(0)
                .map(|(_, content)| content)
                .or_else(|| self.partial_contents.values_mut().next());
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
                let (_, removed_content) = self.contents.pop_front().unwrap();
                drop_bytes -= removed_content.len();
            }
        }

        if let Some(complete_content) = complete_content {
            let prior_pending = self.partial_contents.remove(&stream);
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
                // If the most recent content is from the same job, then just
                // append the pending content and new content to the end

                if let Some(prior_pending) = prior_pending {
                    prior_content.extend_from_slice(&prior_pending);
                }
                prior_content.extend_from_slice(complete_content);
                prior_content.push(b'\n');
            } else {
                // Otherwise, add a new content entry

                let mut bytes = bstr::BString::default();
                if let Some(prior_pending) = prior_pending {
                    bytes.extend_from_slice(&prior_pending);
                }
                bytes.extend_from_slice(complete_content);
                bytes.push(b'\n');

                self.contents.push_back((stream.clone(), bytes));
            }
        }

        if !partial_content.is_empty() {
            match self.partial_contents.entry(stream) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(partial_content.into());
                }
                std::collections::hash_map::Entry::Occupied(entry) => {
                    entry.into_mut().extend_from_slice(partial_content);
                }
            }
        }

        self.total_bytes = match self.max_bytes {
            Some(max_bytes) => std::cmp::min(new_total_bytes, max_bytes),
            None => new_total_bytes,
        };
    }

    pub fn flush_stream(&mut self, stream: K) {
        // Get any partial content that should be flushed
        let Some(partial_content) = self.partial_contents.remove(&stream) else {
            return;
        };

        // We aren't adding or removing any bytes, so no need to truncate
        // or drop old data first

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
            // If the most recent content is from the same job, then just
            // append the flushed content

            prior_content.extend_from_slice(&partial_content);
        } else {
            // Otherwise, add a new content entry

            self.contents.push_back((stream, partial_content));
        }
    }

    pub fn contents(&self) -> impl DoubleEndedIterator<Item = (&K, &bstr::BStr)> {
        self.contents
            .iter()
            .map(|(stream, content)| (stream, bstr::BStr::new(content)))
    }

    pub fn pop_contents(&mut self) -> Option<(K, bstr::BString)> {
        let content = self.contents.pop_front();
        if let Some((_, content)) = &content {
            self.total_bytes = self.total_bytes.saturating_sub(content.len());
        }

        content
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::reporter::job::ProcessStream::{self, Stderr, Stdout};

    type JobOutputBuffer = super::OutputBuffer<JobOutputStream>;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
            output.partial_contents,
            HashMap::from_iter([(job_stream(1, Stdout), "c".into())])
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
            output.partial_contents,
            HashMap::from_iter([
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
    fn test_output_buffer_flush() {
        let mut output = JobOutputBuffer::with_unlimited_capacity();

        output.append(job_stream(1, Stdout), "a\nb\nc");

        assert_eq!(output.total_bytes, 5);
        assert_eq!(output.contents, [(job_stream(1, Stdout), "a\nb\n".into())],);
        assert_eq!(
            output.partial_contents,
            HashMap::from_iter([(job_stream(1, Stdout), "c".into())]),
        );

        output.flush_stream(job_stream(1, Stdout));

        assert_eq!(output.total_bytes, 5);
        assert_eq!(output.contents, [(job_stream(1, Stdout), "a\nb\nc".into())]);
        assert!(output.partial_contents.is_empty());

        output.append(job_stream(1, Stdout), "d\ne\n");

        assert_eq!(
            output.contents,
            [(job_stream(1, Stdout), "a\nb\ncd\ne\n".into())]
        );
        assert!(output.partial_contents.is_empty());
    }

    #[test]
    fn test_output_buffer_pop_contents() {
        let mut contents = JobOutputBuffer::with_unlimited_capacity();

        contents.append(job_stream(1, Stdout), "a\nb\nc");
        contents.append(job_stream(2, Stderr), "d\ne\nf");

        assert_eq!(
            contents.pop_contents(),
            Some((job_stream(1, Stdout), "a\nb\n".into()))
        );

        assert_eq!(contents.total_bytes, 6);
        assert_eq!(
            contents.contents,
            [(job_stream(2, Stderr), "d\ne\n".into())]
        );
        assert_eq!(
            contents.partial_contents,
            [
                (job_stream(1, Stdout), "c".into()),
                (job_stream(2, Stderr), "f".into())
            ]
            .iter()
            .cloned()
            .collect()
        )
    }
}
