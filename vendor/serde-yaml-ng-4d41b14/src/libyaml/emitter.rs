use crate::libyaml::error::Error as LibyamlError;
use libyaml_safer::{
    Emitter as SaferEmitter, Encoding, Event as SaferEvent, MappingStyle,
    ScalarStyle as SaferScalarStyle, SequenceStyle,
};
use std::io;
use std::ptr::NonNull;

#[derive(Debug)]
pub(crate) enum Error {
    Libyaml(LibyamlError),
}

pub(crate) struct Emitter<'a> {
    // The backend borrows the allocation owned by writer_ptr. Destroy the backend
    // before transferring or freeing that allocation, including on error/unwind.
    safer_emitter: Option<SaferEmitter<'a>>,
    writer_ptr: Option<NonNull<dyn io::Write + 'a>>,
}

#[derive(Debug)]
pub(crate) enum Event<'a> {
    StreamStart,
    StreamEnd,
    DocumentStart,
    DocumentEnd,
    Scalar(Scalar<'a>),
    SequenceStart(Sequence),
    SequenceEnd,
    MappingStart(Mapping),
    MappingEnd,
}

#[derive(Debug)]
pub(crate) struct Scalar<'a> {
    pub tag: Option<String>,
    pub value: &'a str,
    pub style: ScalarStyle,
}

#[derive(Debug)]
pub(crate) enum ScalarStyle {
    Any,
    Plain,
    SingleQuoted,
    Literal,
}

#[derive(Debug)]
pub(crate) struct Sequence {
    pub tag: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Mapping {
    pub tag: Option<String>,
}

impl<'a> Emitter<'a> {
    pub fn new(write: Box<dyn io::Write + 'a>) -> Emitter<'a> {
        let mut safer_emitter = SaferEmitter::new();
        safer_emitter.set_unicode(true);
        safer_emitter.set_width(-1);

        let writer_ptr = NonNull::new(Box::into_raw(write)).unwrap();
        // The allocation is stable until into_inner or Drop first destroys the
        // backend. No other reference accesses the writer during that borrow.
        unsafe {
            safer_emitter.set_output(&mut *writer_ptr.as_ptr());
        }

        Emitter {
            safer_emitter: Some(safer_emitter),
            writer_ptr: Some(writer_ptr),
        }
    }

    pub fn emit(&mut self, event: Event) -> Result<(), Error> {
        let safer_event = convert_event(event)?;
        self.safer_emitter
            .as_mut()
            .unwrap()
            .emit(safer_event)
            .map_err(|err| Error::Libyaml(LibyamlError::from_safer_error(err)))
    }

    pub fn flush(&mut self) -> Result<(), Error> {
        self.safer_emitter
            .as_mut()
            .unwrap()
            .flush()
            .map_err(|err| Error::Libyaml(LibyamlError::from_safer_error(err)))
    }

    pub fn into_inner(mut self) -> Box<dyn io::Write + 'a> {
        drop(self.safer_emitter.take());
        let writer = self.writer_ptr.take().unwrap();
        // The backend's unique borrow is gone, and taking the pointer transfers
        // ownership exactly once. Drop will find neither a backend nor a writer.
        unsafe { Box::from_raw(writer.as_ptr()) }
    }
}

impl Drop for Emitter<'_> {
    fn drop(&mut self) {
        drop(self.safer_emitter.take());
        if let Some(writer) = self.writer_ptr.take() {
            // The backend can no longer access this uniquely owned allocation.
            unsafe { drop(Box::from_raw(writer.as_ptr())) }
        }
    }
}

fn convert_event(event: Event) -> Result<SaferEvent, Error> {
    match event {
        Event::StreamStart => Ok(SaferEvent::stream_start(Encoding::Utf8)),
        Event::StreamEnd => Ok(SaferEvent::stream_end()),
        Event::DocumentStart => Ok(SaferEvent::document_start(None, &[], true)),
        Event::DocumentEnd => Ok(SaferEvent::document_end(true)),
        Event::Scalar(scalar) => {
            let style = match scalar.style {
                ScalarStyle::Any => SaferScalarStyle::Any,
                ScalarStyle::Plain => SaferScalarStyle::Plain,
                ScalarStyle::SingleQuoted => SaferScalarStyle::SingleQuoted,
                ScalarStyle::Literal => SaferScalarStyle::Literal,
            };
            Ok(SaferEvent::scalar(
                None,
                scalar.tag.as_deref(),
                scalar.value,
                scalar.tag.is_none(),
                scalar.tag.is_none(),
                style,
            ))
        }
        Event::SequenceStart(sequence) => Ok(SaferEvent::sequence_start(
            None,
            sequence.tag.as_deref(),
            sequence.tag.is_none(),
            SequenceStyle::Any,
        )),
        Event::SequenceEnd => Ok(SaferEvent::sequence_end()),
        Event::MappingStart(mapping) => Ok(SaferEvent::mapping_start(
            None,
            mapping.tag.as_deref(),
            mapping.tag.is_none(),
            MappingStyle::Any,
        )),
        Event::MappingEnd => Ok(SaferEvent::mapping_end()),
    }
}
