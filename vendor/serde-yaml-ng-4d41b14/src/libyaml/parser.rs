use crate::libyaml::debug;
use crate::libyaml::error::{Error, Mark, Result};
use crate::libyaml::tag::Tag;
use crate::libyaml::util::SliceRefStorage;
use libyaml_safer::{
    Encoding, Event as SaferEvent, EventData as SaferEventData, Parser as SaferParser,
    ScalarStyle as SaferScalarStyle,
};
use std::borrow::Cow;
use std::fmt::{self, Debug};

pub(crate) struct Parser<'input> {
    safer_parser: SaferParser<'input>,
    input: Cow<'input, [u8]>,
    // For owned data, store the Box directly so it's automatically freed on drop
    // The reference we get from it will be valid as long as the Box is alive
    _owned_data: Option<Box<[u8]>>,
    // Store the slice reference in a way that allows getting mutable reference
    // This must live as long as the parser so libyaml_safer can modify it
    _slice_storage: SliceRefStorage<'input>,
}

#[derive(Debug)]
pub(crate) enum Event<'input> {
    StreamStart,
    StreamEnd,
    DocumentStart,
    DocumentEnd,
    Alias(Anchor),
    Scalar(Scalar<'input>),
    SequenceStart(SequenceStart),
    SequenceEnd,
    MappingStart(MappingStart),
    MappingEnd,
}

pub(crate) struct Scalar<'input> {
    pub anchor: Option<Anchor>,
    pub tag: Option<Tag>,
    pub value: Box<[u8]>,
    pub style: ScalarStyle,
    pub repr: Option<&'input [u8]>,
}

#[derive(Debug)]
pub(crate) struct SequenceStart {
    pub anchor: Option<Anchor>,
    pub tag: Option<Tag>,
}

#[derive(Debug)]
pub(crate) struct MappingStart {
    pub anchor: Option<Anchor>,
    pub tag: Option<Tag>,
}

#[derive(Ord, PartialOrd, Eq, PartialEq)]
pub(crate) struct Anchor(Box<[u8]>);

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum ScalarStyle {
    Plain,
    SingleQuoted,
    DoubleQuoted,
    Literal,
    Folded,
}

impl<'input> Parser<'input> {
    pub fn new(input: Cow<'input, [u8]>) -> Parser<'input> {
        let mut safer_parser = SaferParser::new();
        safer_parser.set_encoding(Encoding::Utf8);

        // Handle owned vs borrowed data differently
        // For owned data, store the Box directly and get a reference from it
        let (owned_data, input_cow, slice_storage) = match input {
            Cow::Borrowed(b) => {
                // Create storage for the slice reference and get mutable reference
                // The storage must live as long as the parser
                // Parser retains storage, and its backend field drops first.
                // Only that backend accesses the exclusive slice reference.
                let (slice_storage, slice_ref_mut) = unsafe { SliceRefStorage::new(b) };
                // libyaml_safer will store this and modify the inner &[u8] to track progress
                safer_parser.set_input_string(slice_ref_mut);
                (None, Cow::Borrowed(b), slice_storage)
            }
            Cow::Owned(o) => {
                // Convert to Box and store it directly (no leak needed)
                let boxed = o.into_boxed_slice();
                // Get a reference from the Box - this will be valid as long as the Box is alive
                let boxed_ref: &[u8] = &*boxed;
                // Extend lifetime to 'input - safe because we store the Box in the parser
                // and ensure it lives as long as the parser
                let extended: &'input [u8] =
                    unsafe { std::mem::transmute::<&[u8], &'input [u8]>(boxed_ref) };
                // Create storage for the slice reference and get mutable reference
                // The storage must live as long as the parser
                // Parser retains both allocations until its backend drops;
                // owned inputs never expose a borrowed Scalar::repr below.
                let (slice_storage, slice_ref_mut) = unsafe { SliceRefStorage::new(extended) };
                // libyaml_safer will store this and modify the inner &[u8] to track progress
                safer_parser.set_input_string(slice_ref_mut);
                // Store the original input bytes for repr calculation
                let input_cow = Cow::Owned(extended.to_vec());
                (Some(boxed), input_cow, slice_storage)
            }
        };

        Parser {
            safer_parser,
            input: input_cow,
            _owned_data: owned_data,
            _slice_storage: slice_storage,
        }
    }

    pub fn next(&mut self) -> Result<(Event<'input>, Mark)> {
        match self.safer_parser.parse() {
            Ok(safer_event) => {
                let mark = Mark::from_safer_mark(safer_event.start_mark);
                let event = convert_event(safer_event, &self.input)?;
                Ok((event, mark))
            }
            Err(err) => Err(Error::from_safer_error(err)),
        }
    }
}

fn convert_event<'input>(
    safer_event: SaferEvent,
    input: &Cow<'input, [u8]>,
) -> Result<Event<'input>> {
    match safer_event.data {
        SaferEventData::StreamStart { .. } => Ok(Event::StreamStart),
        SaferEventData::StreamEnd => Ok(Event::StreamEnd),
        SaferEventData::DocumentStart { .. } => Ok(Event::DocumentStart),
        SaferEventData::DocumentEnd { .. } => Ok(Event::DocumentEnd),
        SaferEventData::Alias { anchor } => {
            Ok(Event::Alias(Anchor(anchor.into_bytes().into_boxed_slice())))
        }
        SaferEventData::Scalar {
            anchor,
            tag,
            value,
            style,
            ..
        } => {
            let anchor_opt = anchor.map(|a| Anchor(a.into_bytes().into_boxed_slice()));
            let tag_opt = tag.map(|t| Tag(t.into_bytes().into_boxed_slice()));
            let value_bytes = value.into_bytes().into_boxed_slice();
            let style_conv = match style {
                SaferScalarStyle::Plain => ScalarStyle::Plain,
                SaferScalarStyle::SingleQuoted => ScalarStyle::SingleQuoted,
                SaferScalarStyle::DoubleQuoted => ScalarStyle::DoubleQuoted,
                SaferScalarStyle::Literal => ScalarStyle::Literal,
                SaferScalarStyle::Folded => ScalarStyle::Folded,
                SaferScalarStyle::Any => ScalarStyle::Plain,
                _ => ScalarStyle::Plain,
            };
            let repr = if let Cow::Borrowed(input_bytes) = input {
                let start = safer_event.start_mark.index as usize;
                let end = safer_event.end_mark.index as usize;
                if start < input_bytes.len() && end <= input_bytes.len() && start <= end {
                    Some(&input_bytes[start..end])
                } else {
                    None
                }
            } else {
                None
            };
            Ok(Event::Scalar(Scalar {
                anchor: anchor_opt,
                tag: tag_opt,
                value: value_bytes,
                style: style_conv,
                repr,
            }))
        }
        SaferEventData::SequenceStart { anchor, tag, .. } => {
            let anchor_opt = anchor.map(|a| Anchor(a.into_bytes().into_boxed_slice()));
            let tag_opt = tag.map(|t| Tag(t.into_bytes().into_boxed_slice()));
            Ok(Event::SequenceStart(SequenceStart {
                anchor: anchor_opt,
                tag: tag_opt,
            }))
        }
        SaferEventData::SequenceEnd => Ok(Event::SequenceEnd),
        SaferEventData::MappingStart { anchor, tag, .. } => {
            let anchor_opt = anchor.map(|a| Anchor(a.into_bytes().into_boxed_slice()));
            let tag_opt = tag.map(|t| Tag(t.into_bytes().into_boxed_slice()));
            Ok(Event::MappingStart(MappingStart {
                anchor: anchor_opt,
                tag: tag_opt,
            }))
        }
        SaferEventData::MappingEnd => Ok(Event::MappingEnd),
    }
}

impl Debug for Scalar<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        let Scalar {
            anchor,
            tag,
            value,
            style,
            repr: _,
        } = self;

        struct LossySlice<'a>(&'a [u8]);

        impl Debug for LossySlice<'_> {
            fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                debug::debug_lossy(self.0, formatter)
            }
        }

        formatter
            .debug_struct("Scalar")
            .field("anchor", anchor)
            .field("tag", tag)
            .field("value", &LossySlice(value))
            .field("style", style)
            .finish()
    }
}

impl Debug for Anchor {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        debug::debug_lossy(&self.0, formatter)
    }
}
