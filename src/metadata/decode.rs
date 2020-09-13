// TODO: documentation

use core::{convert::TryFrom as _, fmt, str};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct MetadataRef<'a> {
    pub modules: UndecodedIter<'a, ModuleMetadataRef<'a>>,
    pub extrinsic: ExtrinsicMetadataRef<'a>,
}

impl<'a> MetadataRef<'a> {
    // TODO: move this as a freestanding function
    pub fn from_slice(data: &'a [u8]) -> Result<Self, DecodeError> {
        let (_remain, out) =
            nom::combinator::all_consuming(opaque_metadata)(data).map_err(DecodeError)?;
        debug_assert!(_remain.is_empty());
        Ok(out)
    }
}

/// All metadata about an runtime module.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ModuleMetadataRef<'a> {
    pub name: &'a str,
    pub storage: Option<StorageMetadataRef<'a>>,
    pub calls: Option<UndecodedIter<'a, FunctionMetadataRef<'a>>>,
    pub event: Option<UndecodedIter<'a, EventMetadataRef<'a>>>,
    pub constants: UndecodedIter<'a, ModuleConstantMetadataRef<'a>>,
    pub errors: UndecodedIter<'a, ErrorMetadataRef<'a>>,
}

/// All metadata of the storage.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct StorageMetadataRef<'a> {
    /// The common prefix used by all storage entries.
    pub prefix: &'a str,
    pub entries: UndecodedIter<'a, StorageEntryMetadataRef<'a>>,
}

/// All the metadata about one storage entry.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct StorageEntryMetadataRef<'a> {
    pub name: &'a str,
    pub modifier: StorageEntryModifier,
    pub ty: StorageEntryTypeRef<'a>,
    pub default: &'a [u8],
    pub documentation: UndecodedIter<'a, &'a str>,
}

/// A storage entry modifier.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum StorageEntryModifier {
    Optional,
    Default,
}

/// A storage entry type.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum StorageEntryTypeRef<'a> {
    Plain(&'a str),
    Map {
        hasher: StorageHasher,
        key: &'a str,
        value: &'a str,
    },
    DoubleMap {
        hasher: StorageHasher,
        key1: &'a str,
        key2: &'a str,
        value: &'a str,
        key2_hasher: StorageHasher,
    },
}

/// Hasher used by storage maps
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum StorageHasher {
    Blake2_128,
    Blake2_256,
    Blake2_128Concat,
    Twox128,
    Twox256,
    Twox64Concat,
    Identity,
}

/// All the metadata about a function.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FunctionMetadataRef<'a> {
    pub name: &'a str,
    pub arguments: UndecodedIter<'a, FunctionArgumentMetadataRef<'a>>,
    pub documentation: UndecodedIter<'a, &'a str>,
}

/// All the metadata about a function argument.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct FunctionArgumentMetadataRef<'a> {
    pub name: &'a str,
    pub ty: &'a str,
}

/// All the metadata about an event.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct EventMetadataRef<'a> {
    pub name: &'a str,
    pub arguments: UndecodedIter<'a, &'a str>,
    pub documentation: UndecodedIter<'a, &'a str>,
}

/// All the metadata about one module constant.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ModuleConstantMetadataRef<'a> {
    pub name: &'a str,
    pub ty: &'a str,
    pub value: &'a [u8],
    pub documentation: UndecodedIter<'a, &'a str>,
}

/// All the metadata about a module error.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ErrorMetadataRef<'a> {
    pub name: &'a str,
    pub documentation: UndecodedIter<'a, &'a str>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ExtrinsicMetadataRef<'a> {
    /// Extrinsic version.
    pub version: u8,
    /// The signed extensions in the order they appear in the extrinsic.
    pub signed_extensions: UndecodedIter<'a, &'a str>,
}

/// Error that can happen during the decoding.
#[derive(Debug, derive_more::Display)]
pub struct DecodeError<'a>(nom::Err<NomError<'a>>);

/// `nom` error type that we use. Can be changed for a `nom::error::VerboseError<&'a [u8]>` for
/// more verbose error messages.
type NomError<'a> = (&'a [u8], nom::error::ErrorKind);

pub struct UndecodedIter<'a, T> {
    bytes: &'a [u8],
    num_items: usize,
    decoding_fn: fn(&'a [u8]) -> nom::IResult<&'a [u8], T, NomError<'a>>,
}

impl<'a, T> Iterator for UndecodedIter<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        if self.num_items == 0 {
            return None;
        }

        let (rest, item) = (self.decoding_fn)(self.bytes).unwrap();
        self.bytes = rest;
        self.num_items -= 1;
        Some(item)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.num_items, Some(self.num_items))
    }
}

impl<'a, T> ExactSizeIterator for UndecodedIter<'a, T> {}

// A manual implementation of `Clone` is necessary, as automatic derivation generates an
// unnecessary `T: Clone` bound.
impl<'a, T> Clone for UndecodedIter<'a, T> {
    fn clone(&self) -> Self {
        UndecodedIter {
            bytes: self.bytes,
            num_items: self.num_items,
            decoding_fn: self.decoding_fn,
        }
    }
}

impl<'a, T> Copy for UndecodedIter<'a, T> {}

impl<'a, T> fmt::Debug for UndecodedIter<'a, T>
where
    T: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(*self).finish()
    }
}

impl<'a, T> PartialEq for UndecodedIter<'a, T>
where
    T: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        let mut iter1 = self.clone();
        let mut iter2 = other.clone();
        loop {
            match (iter1.next(), iter2.next()) {
                (None, None) => return true,
                (Some(a), Some(b)) if a == b => {}
                _ => return false,
            }
        }
    }
}

impl<'a, T> Eq for UndecodedIter<'a, T> where T: Eq {}

// `nom` parser functions can be found below.

fn opaque_metadata(bytes: &[u8]) -> nom::IResult<&[u8], MetadataRef, NomError> {
    // For some undeterminate reason, the metadata starts with a SCALE-compact length of the rest
    // of the metadata.
    nom::multi::length_value(
        scale_compact_usize,
        nom::combinator::all_consuming(prefixed_metadata),
    )(bytes)
}

fn prefixed_metadata(bytes: &[u8]) -> nom::IResult<&[u8], MetadataRef, NomError> {
    nom::sequence::preceded(
        // This tag exists to intentionally generate a parsing error if endianness is
        // badly handled.
        nom::error::context(
            "endianess tag",
            nom::bytes::complete::tag(&[0x6d, 0x65, 0x74, 0x61]),
        ),
        metadata,
    )(bytes)
}

fn metadata(bytes: &[u8]) -> nom::IResult<&[u8], MetadataRef, NomError> {
    nom::combinator::map(
        nom::sequence::preceded(
            nom::error::context("version number", nom::bytes::complete::tag(&[11])), // version number
            nom::sequence::pair(|i| vec_decode(i, module_metadata), extrinsic_metadata),
        ),
        |(modules, extrinsic)| MetadataRef { modules, extrinsic },
    )(bytes)
}

fn module_metadata(bytes: &[u8]) -> nom::IResult<&[u8], ModuleMetadataRef, NomError> {
    nom::error::context(
        "module",
        nom::combinator::map(
            nom::sequence::tuple((
                string_decode,
                |i| option_decode(i, storage_metadata),
                |i| option_decode(i, |i| vec_decode(i, function_metadata)),
                |i| option_decode(i, |i| vec_decode(i, event_metadata)),
                |i| vec_decode(i, module_constant_metadata),
                |i| vec_decode(i, error_metadata),
            )),
            |(name, storage, calls, event, constants, errors)| ModuleMetadataRef {
                name,
                storage,
                calls,
                event,
                constants,
                errors,
            },
        ),
    )(bytes)
}

fn storage_metadata(bytes: &[u8]) -> nom::IResult<&[u8], StorageMetadataRef, NomError> {
    nom::error::context(
        "storage",
        nom::combinator::map(
            nom::sequence::tuple((string_decode, |i| vec_decode(i, storage_entry_metadata))),
            |(prefix, entries)| StorageMetadataRef { prefix, entries },
        ),
    )(bytes)
}

fn storage_entry_metadata(bytes: &[u8]) -> nom::IResult<&[u8], StorageEntryMetadataRef, NomError> {
    nom::error::context(
        "storage entry",
        nom::combinator::map(
            nom::sequence::tuple((
                string_decode,
                storage_entry_modifier,
                storage_entry_type,
                bytes_decode,
                |i| vec_decode(i, string_decode),
            )),
            |(name, modifier, ty, default, documentation)| StorageEntryMetadataRef {
                name,
                modifier,
                ty,
                default,
                documentation,
            },
        ),
    )(bytes)
}

fn storage_entry_modifier(bytes: &[u8]) -> nom::IResult<&[u8], StorageEntryModifier, NomError> {
    nom::error::context(
        "storage entry modifier",
        nom::branch::alt((
            nom::combinator::map(nom::bytes::complete::tag(&[0]), |_| {
                StorageEntryModifier::Optional
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[1]), |_| {
                StorageEntryModifier::Default
            }),
        )),
    )(bytes)
}

fn storage_entry_type(bytes: &[u8]) -> nom::IResult<&[u8], StorageEntryTypeRef, NomError> {
    nom::error::context(
        "storage entry type",
        nom::branch::alt((
            nom::combinator::map(
                nom::sequence::preceded(nom::bytes::complete::tag(&[0]), string_decode),
                StorageEntryTypeRef::Plain,
            ),
            nom::combinator::map(
                nom::sequence::preceded(
                    nom::bytes::complete::tag(&[1]),
                    nom::sequence::tuple((
                        storage_hasher,
                        string_decode,
                        string_decode,
                        nom::bytes::complete::take(1u32),
                    )),
                ),
                |(hasher, key, value, _unused)| StorageEntryTypeRef::Map { hasher, key, value },
            ),
            nom::combinator::map(
                nom::sequence::preceded(
                    nom::bytes::complete::tag(&[2]),
                    nom::sequence::tuple((
                        storage_hasher,
                        string_decode,
                        string_decode,
                        string_decode,
                        storage_hasher,
                    )),
                ),
                |(hasher, key1, key2, value, key2_hasher)| StorageEntryTypeRef::DoubleMap {
                    hasher,
                    key1,
                    key2,
                    value,
                    key2_hasher,
                },
            ),
        )),
    )(bytes)
}

fn storage_hasher(bytes: &[u8]) -> nom::IResult<&[u8], StorageHasher, NomError> {
    nom::error::context(
        "storage hasher",
        nom::branch::alt((
            nom::combinator::map(nom::bytes::complete::tag(&[0]), |_| {
                StorageHasher::Blake2_128
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[1]), |_| {
                StorageHasher::Blake2_256
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[2]), |_| {
                StorageHasher::Blake2_128Concat
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[3]), |_| StorageHasher::Twox128),
            nom::combinator::map(nom::bytes::complete::tag(&[4]), |_| StorageHasher::Twox256),
            nom::combinator::map(nom::bytes::complete::tag(&[5]), |_| {
                StorageHasher::Twox64Concat
            }),
            nom::combinator::map(nom::bytes::complete::tag(&[6]), |_| StorageHasher::Identity),
        )),
    )(bytes)
}

fn function_metadata(bytes: &[u8]) -> nom::IResult<&[u8], FunctionMetadataRef, NomError> {
    nom::error::context(
        "function",
        nom::combinator::map(
            nom::sequence::tuple((
                string_decode,
                |i| vec_decode(i, function_argument_metadata),
                |i| vec_decode(i, string_decode),
            )),
            |(name, arguments, documentation)| FunctionMetadataRef {
                name,
                arguments,
                documentation,
            },
        ),
    )(bytes)
}

fn function_argument_metadata(
    bytes: &[u8],
) -> nom::IResult<&[u8], FunctionArgumentMetadataRef, NomError> {
    nom::error::context(
        "function argument",
        nom::combinator::map(
            nom::sequence::tuple((string_decode, string_decode)),
            |(name, ty)| FunctionArgumentMetadataRef { name, ty },
        ),
    )(bytes)
}

fn event_metadata(bytes: &[u8]) -> nom::IResult<&[u8], EventMetadataRef, NomError> {
    nom::error::context(
        "event",
        nom::combinator::map(
            nom::sequence::tuple((
                string_decode,
                |i| vec_decode(i, string_decode),
                |i| vec_decode(i, string_decode),
            )),
            |(name, arguments, documentation)| EventMetadataRef {
                name,
                arguments,
                documentation,
            },
        ),
    )(bytes)
}

fn module_constant_metadata(
    bytes: &[u8],
) -> nom::IResult<&[u8], ModuleConstantMetadataRef, NomError> {
    nom::error::context(
        "constant",
        nom::combinator::map(
            nom::sequence::tuple((string_decode, string_decode, bytes_decode, |i| {
                vec_decode(i, string_decode)
            })),
            |(name, ty, value, documentation)| ModuleConstantMetadataRef {
                name,
                ty,
                value,
                documentation,
            },
        ),
    )(bytes)
}

fn error_metadata(bytes: &[u8]) -> nom::IResult<&[u8], ErrorMetadataRef, NomError> {
    nom::error::context(
        "error",
        nom::combinator::map(
            nom::sequence::pair(string_decode, |i| vec_decode(i, string_decode)),
            |(name, documentation)| ErrorMetadataRef {
                name,
                documentation,
            },
        ),
    )(bytes)
}

fn extrinsic_metadata(bytes: &[u8]) -> nom::IResult<&[u8], ExtrinsicMetadataRef, NomError> {
    nom::error::context(
        "extrinsic",
        nom::combinator::map(
            nom::sequence::pair(nom::bytes::complete::take(1u32), |i| {
                vec_decode(i, string_decode)
            }),
            |(version, signed_extensions)| ExtrinsicMetadataRef {
                version: version[0],
                signed_extensions,
            },
        ),
    )(bytes)
}

// TODO: functions below are generic and could be moved somewhere else?

/// Decodes a SCALE-encoded vec of bytes.
fn bytes_decode<'a>(bytes: &'a [u8]) -> nom::IResult<&'a [u8], &'a [u8], NomError<'a>> {
    nom::multi::length_data(scale_compact_usize)(bytes)
}

/// Decodes a SCALE-encoded string.
fn string_decode<'a>(bytes: &'a [u8]) -> nom::IResult<&'a [u8], &'a str, NomError<'a>> {
    nom::combinator::map_res(nom::multi::length_data(scale_compact_usize), str::from_utf8)(bytes)
}

/// Decodes a SCALE-encoded `Vec`.
fn vec_decode<'a, O>(
    bytes: &'a [u8],
    decoding_fn: fn(&'a [u8]) -> nom::IResult<&'a [u8], O, NomError<'a>>,
) -> nom::IResult<&'a [u8], UndecodedIter<'a, O>, NomError<'a>> {
    let (value_start, num_items) = scale_compact_usize(bytes)?;

    let mut verify_iter = value_start;
    for _ in 0..num_items {
        let (remain, _) = decoding_fn(verify_iter)?;
        verify_iter = remain;
    }

    Ok((
        verify_iter,
        UndecodedIter {
            bytes,
            num_items,
            decoding_fn,
        },
    ))
}

/// Decodes a SCALE-encoded `Option`.
fn option_decode<'a, O, E: nom::error::ParseError<&'a [u8]>>(
    bytes: &'a [u8],
    inner_decode: impl Fn(&'a [u8]) -> nom::IResult<&'a [u8], O, E>,
) -> nom::IResult<&'a [u8], Option<O>, E> {
    nom::branch::alt((
        nom::combinator::map(nom::bytes::complete::tag(&[0]), |_| None),
        nom::combinator::map(
            nom::sequence::preceded(nom::bytes::complete::tag(&[1]), inner_decode),
            Some,
        ),
    ))(bytes)
}

/// Decodes a SCALE-compact-encoded usize.
fn scale_compact_usize<'a, E: nom::error::ParseError<&'a [u8]>>(
    bytes: &'a [u8],
) -> nom::IResult<&'a [u8], usize, E> {
    if bytes.is_empty() {
        return Err(nom::Err::Error(nom::error::make_error(
            bytes,
            nom::error::ErrorKind::Eof,
        )));
    }

    match bytes[0] & 0b11 {
        0b00 => {
            let value = bytes[0] >> 2;
            Ok((&bytes[1..], usize::from(value)))
        }
        0b01 => {
            if bytes.len() < 2 {
                return Err(nom::Err::Error(nom::error::make_error(
                    bytes,
                    nom::error::ErrorKind::Eof,
                )));
            }

            let byte0 = u16::from(bytes[0] >> 2);
            let byte1 = u16::from(bytes[1]);
            let value = (byte1 << 6) | byte0;
            Ok((&bytes[2..], usize::from(value)))
        }
        0b10 => {
            if bytes.len() < 4 {
                return Err(nom::Err::Error(nom::error::make_error(
                    bytes,
                    nom::error::ErrorKind::Eof,
                )));
            }

            let byte0 = u32::from(bytes[0] >> 2);
            let byte1 = u32::from(bytes[1]);
            let byte2 = u32::from(bytes[2]);
            let byte3 = u32::from(bytes[3]);
            let value = (byte3 << 22) | (byte2 << 14) | (byte1 << 6) | byte0;
            let value = match usize::try_from(value) {
                Ok(v) => v,
                Err(_) => todo!(), // TODO:
            };
            Ok((&bytes[4..], value))
        }
        0b11 => todo!(), // TODO:
        _ => unreachable!(),
    }
}
