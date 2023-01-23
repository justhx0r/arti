//! Building support for the network document meta-format
//!
//! Implements building documents according to
//! [dir-spec.txt](https://spec.torproject.org/dir-spec).
//! section 1.2 and 1.3.
//!
//! This facility proces output that complies with the meta-document format,
//! (`dir-spec.txt` section 1.2) -
//! unless `raw` methods are called with improper input.
//!
//! However, no checks are done on keyword presence/absence, multiplicity, or ordering,
//! so the output may not necessarily conform to the format of the particular intended document.
//! It is the caller's responsibility to call `.item()` in the right order,
//! with the right keywords and arguments.

#![allow(unused_variables)] // TODO hs
#![allow(unused_imports)] // TODO hs
#![allow(dead_code)] // TODO hs
#![allow(clippy::missing_docs_in_private_items)] // TODO hs
#![allow(clippy::needless_pass_by_value)] // TODO hs

use std::fmt::Display;
use std::marker::PhantomData;
use std::ops::Deref;

use tor_error::Bug;

use crate::parse::keyword::Keyword;

/// Network document text according to dir-spec.txt s1.2 and maybe s1.3
///
/// Contains just the text, but marked with the type of the builder
/// for clarity in function signatures etc.
pub struct NetdocText<Builder> {
    text: String,
    // variance: this somehow came from a T (not that we expect this to matter)
    kind: PhantomData<Builder>,
}

impl<B> Deref for NetdocText<B> {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}

/// Encoder, representing a partially-built document
///
/// # Example
///
/// ```ignore
/// # TODO hs
/// ```
pub(crate) struct NetdocEncoder {
    // Err means bad values passed to some builder function
    built: Result<String, Bug>,
}

/// Encoder for an individual item within a being-built document
///
/// Returned by [`NetdocEncoder::item()`].
// we need to accumulate these in pieces, and put them in doc later,
// because otherwise args and object can't be specified in any order
// and we'd need a typestate, and also there's the newline after the
// args
pub(crate) struct ItemEncoder<'n> {
    // keyword: K, // TODO hs: remove this
    /// `None` after `drop`, or if an error occurred
    doc: Option<&'n mut NetdocEncoder>,
    args: Vec<String>,
    /// Encoded form of the zero or one Object
    ///
    /// Includes all necessary framing includng trailing newlines.
    /// Empty if there is no object.
    object: String,
}

/// Position within a (perhaps partially-) built document
///
/// This is provided mainly to allow the caller to perform signature operations
/// on the part of the document that is to be signed.
/// (Sometimes this is only part of it.)
pub(crate) struct Cursor {
    offset: usize,
    // Actually, we don't want cursors to be statically typed by keyword, so K generic dropped
    // Variance: notionally refers to a keyword K
    // marker: PhantomData<*const K>,
}

/// Types that can be added as argument(s) to item keyword lines
///
/// Implemented for strings, and various other types.
///
/// This is a separate trait so we can control the formatting of (eg) [`SystemTime`],
/// without having a method on `ItemEncoder` for each argument type.
pub(crate) trait ItemArgument {
    /// Format as a string suitable for including as a netdoc keyword line argument
    ///
    /// The implementation is responsible for checking that the syntax is legal.
    /// For example, if `self` is a string, it must check that the string is
    /// in legal as a single argument.
    ///
    /// Some netdoc values (eg times) turn into several arguments; in that case,
    /// one `ItemArgument` may format into multiple arguments, and this method
    /// is responsible for writing them all, with the necessary spaces.
    fn write_onto(&self, out: &mut ItemEncoder<'_>) -> Result<(), Bug>;
}

impl NetdocEncoder {
    /// Adds an item to the being-built document
    ///
    /// The item can be further extended with arguments or an object,
    /// using the returned `ItemEncoder`.
    //
    // Actually, we defer adding the item until `ItemEncoder` is dropped.
    pub(crate) fn item(&mut self, keyword: impl Keyword) -> ItemEncoder {
        todo!()
    }

    /// Adds raw text to the being-built document
    ///
    /// `s` is added as raw text, after the newline ending the previous item.
    /// If `item` is subsequently called, the start of that item
    /// will immediately follow `s`.
    ///
    /// It is the responsibility of the caller to obey the metadocument syntax.
    /// In particular, `s` should end with a newline.
    /// No checks are performed.
    /// Incorrect use might lead to malformed documents, or later errors.
    pub(crate) fn push_raw_string(&mut self, s: &dyn Display) {
        todo!()
    }

    pub(crate) fn cursor(&self) -> Cursor {
        todo!()
    }

    /// Obtain the text of a section of the document
    ///
    /// Useful for making a signature.
    //
    // Q. Should this return `&str` or `NetdocText<'self>` ?
    // (`NetdocText would have to then contain `Cow`, which is fine.)
    pub(crate) fn slice(&self, begin: Cursor, end: Cursor) -> Result<&str, Bug> {
        todo!()
    }

    /// Build the document into textual form
    pub(crate) fn finish() -> Result<NetdocText<Self>, Bug> {
        todo!()
    }
}

impl ItemArgument for str {
    fn write_onto(&self, out: &mut ItemEncoder<'_>) -> Result<(), Bug> {
        todo!()
    }
}

// TODO hs:
// impl<T: ItemArgument + ?Sized> ItemArgument for &'_ T
// impl ItemArgument for usize, etc.
// impl ItemArgument for SystemTime

impl<'n> ItemEncoder<'n> {
    /// Add a single argument.
    ///
    /// If the argument is not in the correct syntax, a `Bug`
    /// error will be reported (later).
    // This is not a hot path.  `dyn` for smaller code size.
    //
    // If arg is not in the correct syntax, a `Bug` is stored in self.doc.
    pub(crate) fn arg(mut self, arg: &dyn ItemArgument) -> Self {
        self.add_arg(arg);
        self
    }

    /// Add a single argument, to a borrowed `ItemEncoder`
    ///
    /// If the argument is not in the correct syntax, a `Bug`
    /// error will be reported (later).
    //
    // Needed for implementing `ItemArgument`
    pub(crate) fn add_arg(&mut self, arg: &dyn ItemArgument) {
        todo!()
    }

    /// Add zero or more arguments, supplied as a single string.
    ///
    /// `args` should zero or more valid argument strings,
    /// separated by (single) spaces.
    /// This is not (properly) checked.
    /// Incorrect use might lead to malformed documents, or later errors.
    pub(crate) fn args_raw_string(self, args: &dyn Display) -> Self {
        todo!()
    }

    // If keyword is not in the correct syntax,
    // or data fails to be written, a `Bug` is stored in self.doc.
    pub(crate) fn object(
        self,
        keyword: &str,
        // Writeable isn't dyn-compatible
        data: impl tor_bytes::WriteableOnce,
    ) {
        todo!()
    }
}

impl Drop for ItemEncoder<'_> {
    fn drop(&mut self) {
        // actually add any not-yet-flushed parts of the item
        // to self.doc.built.
    }
}
