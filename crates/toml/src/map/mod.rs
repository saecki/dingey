//! ## Structure
//! - [MapTable] // root
//!     - reprs: [OneVec<MapTableRepr>]
//!         - [MapTableRepr::Root]
//!     - entries:
//!         - "children_1": [MapTableEntry]
//!             - reprs: [OneVec<MapTableEntryRepr>]
//!                 - parent: [ParentTable] // refers to root
//!                 - key: ...
//!                 - kind: ...
//!             - node: [MapNode::Table]: [MapTable]
//!                 - reprs: [OneVec<MapTableRepr>]
//!                     - ...
//!                 - entries:
//!                     - "node_1": [MapTableEntry]
//!                         - node: [MapNode::Scalar]
//!                         - reprs: [OneVec<MapTableEntryRepr>]
//!                             - parent: [ParentEntry::Table]
//!                             - key: ...
//!                             - kind: ...
//!                     - "node_2": [MapTableEntry]
//!                         - node: [MapNode::Scalar]
//!                         - reprs: [OneVec<MapTableEntryRepr>]
//!                             - parent: [ParentEntry::Table]
//!                             - key: ...
//!                             - kind: ...
//!
//! ## Possible representations of the above
//! ```toml
//! # 1
//! [children_1]
//! node_1 = 1
//! node_2 = false
//!
//! # 2
//! children_1.node_1 = 1
//! children_1.node_2 = false
//!
//! # 3
//! children_1 = { node_1 = 1, node_2 = false }
//! ```
// FIXME: All collections that allocate outside of the bump allocator will leak.

use bumpalo::Bump;
use common::OneVec;
use common::Span;

use crate::map::construct::{MapError, MapErrorKind};
use crate::map::cyclic::{Complete, Cyclic, CyclicCell};
use crate::map::parent::{
    ParentEntry, ParentInlineArray, ParentInlineArrayEntry, ParentTable, ParentTableEntry,
    ParentToplevelArray, ParentToplevelArrayEntry, ReprIdx,
};
use crate::parse::{
    ArrayEntry, BoolVal, CommentRange, DateTimeVal, DottedIdent, FloatVal, Ident, InlineArray,
    InlineArrayValue, InlineTable, InlineTableAssignment, IntVal, StringVal, Table,
    ToplevelAssignment,
};
use crate::{Ast, Error, TomlCtx};

pub use path::*;

mod construct;
pub mod cyclic;
pub mod parent;
mod path;
#[cfg(test)]
mod test;

type MapInner<'a, S = Complete> =
    indexmap::IndexMap<&'a str, CyclicCell<'a, MapTableEntry<'a, S>, S>>;

#[derive(Debug, PartialEq)]
pub struct MapTable<'a, S = Complete> {
    inner: MapInner<'a, S>,
    pub reprs: OneVec<MapTableRepr<'a, S>>,
}

impl<'a, S> MapTable<'a, S> {
    pub fn new(repr: MapTableRepr<'a, S>) -> Self {
        Self {
            reprs: OneVec::new(repr),
            inner: MapInner::new(),
        }
    }
}

impl<'a> MapTable<'a, Complete> {
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&'a MapTableEntry<'a>> {
        self.inner.get(key).map(|e| e.get())
    }

    pub fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = (&'a str, &'a MapTableEntry<'a>)> + DoubleEndedIterator {
        self.inner.iter().map(|(key, entry)| (*key, entry.get()))
    }

    pub(crate) fn as_slice(
        &self,
    ) -> &indexmap::map::Slice<&'a str, CyclicCell<'a, MapTableEntry<'a>>> {
        self.inner.as_slice()
    }
}

/// All possible ast definitions, that can make up a table.
#[derive(Clone, Debug, PartialEq)]
pub enum MapTableRepr<'a, S = Complete> {
    /// The entire file. This is the root table.
    Root(Span),
    /// This table would be part of the representation of the `a` and `a.table`
    /// tables.
    /// ```toml
    /// [a.table]
    /// ...
    /// ```
    Table(&'a Table<'a>, ParentEntry<'a, S>),
    /// ```toml
    /// an.inline.table = { }
    /// #                 ^^^ this part here
    /// ```
    InlineTable(&'a InlineTable<'a>, ParentEntry<'a, S>),
    /// This array of tables entry would be part of the represenation of the
    /// `an` and `an.array` tables. The `an.array.entry` key corresponds to
    /// a [`MapArray::Toplevel`], which in will contain a [`MapArrayToplevelEntry`]
    /// for this definition.
    /// ```toml
    /// [[an.array.entry]]
    /// ...
    /// ```
    ArrayEntry(&'a ArrayEntry<'a>, ParentEntry<'a, S>),
    /// This assignment would be part of the represenation for the `a` table and
    /// the `a.toplevel` table.
    ///
    /// Note that only assignments with dotted keys can make up a table.
    /// And the last identifier of the dotted key, cannot be part of the
    /// representation that makes up a [`MapTableRepr`], but it will represent
    /// a [`MapTableEntryRepr`].
    /// ```toml
    /// a.toplevel.assignment = 12
    /// ```
    ToplevelAssignment(&'a ToplevelAssignment<'a>, ParentEntry<'a, S>),
    /// Both assignments inside the inline table would make up part of the
    /// `things.table` table.
    /// ```toml
    /// things = { table.a = 12, table.b = 13 }
    ///            ^^^^^ here    ^^^^^ and here
    /// ```
    InlineTableAssignment(&'a InlineTableAssignment<'a>, ParentEntry<'a, S>),
}

impl<'a, S> MapTableRepr<'a, S> {
    pub fn span(&self) -> Span {
        match self {
            MapTableRepr::Root(span) => *span,
            MapTableRepr::Table(table, _) => table.span(),
            MapTableRepr::InlineTable(table, _) => table.span(),
            MapTableRepr::ArrayEntry(array, _) => array.span(),
            MapTableRepr::ToplevelAssignment(assignment, _) => assignment.span(),
            MapTableRepr::InlineTableAssignment(assignment, _) => assignment.span(),
        }
    }

    pub fn parent_entry(&self) -> Option<ParentEntry<'a, S>>
    where
        S: Copy,
    {
        match self {
            MapTableRepr::Root(_) => None,
            MapTableRepr::Table(_, parent_entry) => Some(*parent_entry),
            MapTableRepr::InlineTable(_, parent_entry) => Some(*parent_entry),
            MapTableRepr::ArrayEntry(_, parent_entry) => Some(*parent_entry),
            MapTableRepr::ToplevelAssignment(_, parent_entry) => Some(*parent_entry),
            MapTableRepr::InlineTableAssignment(_, parent_entry) => Some(*parent_entry),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct MapTableEntry<'a, S = Complete> {
    pub node: MapNode<'a, S>,
    /// References to the actual representations inside the toml file.
    pub reprs: OneVec<MapTableEntryRepr<'a, S>>,
}

impl<'a, S> MapTableEntry<'a, S> {
    fn new(node: MapNode<'a, S>, repr: MapTableEntryRepr<'a, S>) -> Self {
        Self {
            node,
            reprs: OneVec::new(repr),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapTableEntryRepr<'a, S = Complete> {
    /// Index of the parent defined in the parent [`MapTableEntry::reprs`].
    pub parent: ParentTable<'a, S>,
    pub key: MapTableKeyRepr<'a>,
    pub kind: MapTableEntryReprKind<'a>,
}

impl<'a, S> MapTableEntryRepr<'a, S> {
    fn new(
        parent: ParentTable<'a, S>,
        key: MapTableKeyRepr<'a>,
        kind: MapTableEntryReprKind<'a>,
    ) -> Self {
        Self { parent, key, kind }
    }

    /// The span from the [`MapTableKeyRepr::repr_ident`] to the end of the value.
    pub fn repr_span(&self) -> Span {
        Span::new(self.key.repr_ident().lit_start, self.kind.span().end)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MapTableEntryReprKind<'a> {
    Table(&'a Table<'a>),
    ArrayEntry(&'a ArrayEntry<'a>),
    ToplevelAssignment(&'a ToplevelAssignment<'a>),
    InlineTableAssignment(&'a InlineTableAssignment<'a>),
}

impl<'a> MapTableEntryReprKind<'a> {
    #[inline]
    pub fn span(&self) -> Span {
        match self {
            MapTableEntryReprKind::Table(t) => t.span(),
            MapTableEntryReprKind::ArrayEntry(a) => a.span(),
            MapTableEntryReprKind::ToplevelAssignment(a) => a.span(),
            MapTableEntryReprKind::InlineTableAssignment(a) => a.span(),
        }
    }

    #[inline]
    pub fn comments(&self) -> Option<CommentRange> {
        match self {
            MapTableEntryReprKind::Table(t) => Some(t.comments),
            MapTableEntryReprKind::ArrayEntry(a) => Some(a.comments),
            MapTableEntryReprKind::ToplevelAssignment(a) => Some(a.comments),
            MapTableEntryReprKind::InlineTableAssignment(_) => None,
        }
    }

    #[inline]
    pub fn is_assignment(&self) -> bool {
        match self {
            MapTableEntryReprKind::Table(_) => false,
            MapTableEntryReprKind::ArrayEntry(_) => false,
            MapTableEntryReprKind::ToplevelAssignment(_) => true,
            MapTableEntryReprKind::InlineTableAssignment(_) => true,
        }
    }

    pub fn table_repr<S>(&self, parent_entry: ParentEntry<'a, S>) -> MapTableRepr<'a, S> {
        match self {
            MapTableEntryReprKind::Table(table) => MapTableRepr::Table(table, parent_entry),
            MapTableEntryReprKind::ArrayEntry(array) => {
                MapTableRepr::ArrayEntry(array, parent_entry)
            }
            MapTableEntryReprKind::ToplevelAssignment(assignment) => {
                MapTableRepr::ToplevelAssignment(assignment, parent_entry)
            }
            MapTableEntryReprKind::InlineTableAssignment(assignment) => {
                MapTableRepr::InlineTableAssignment(assignment, parent_entry)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MapTableKeyRepr<'a> {
    One(&'a Ident<'a>),
    Dotted(u32, &'a [DottedIdent<'a>]),
}

impl<'a> MapTableKeyRepr<'a> {
    /// The [`Ident`] responsible for declaring a table, might be an identifier in a dotted key.
    pub fn repr_ident(&self) -> &'a Ident<'a> {
        match self {
            MapTableKeyRepr::One(i) => i,
            MapTableKeyRepr::Dotted(idx, idents) => &idents[*idx as usize].ident,
        }
    }

    pub fn is_last_ident(&self) -> bool {
        match self {
            MapTableKeyRepr::One(_) => true,
            MapTableKeyRepr::Dotted(idx, idents) => *idx as usize == idents.len() - 1,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum MapArray<'a, S = Complete> {
    Toplevel(CyclicCell<'a, MapArrayToplevel<'a, S>, S>),
    Inline(&'a MapArrayInline<'a, S>),
}

#[derive(Debug, PartialEq)]
pub struct MapArrayToplevel<'a, S = Complete> {
    inner: OneVec<CyclicCell<'a, MapArrayToplevelEntry<'a, S>, S>>,
}

impl<'a, S> MapArrayToplevel<'a, S> {
    pub fn new(entry: CyclicCell<'a, MapArrayToplevelEntry<'a, S>, S>) -> Self {
        Self {
            inner: OneVec::new(entry),
        }
    }
}

impl<'a> MapArrayToplevel<'a, Complete> {
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }

    pub(crate) fn as_slice(&self) -> &[CyclicCell<'a, MapArrayToplevelEntry<'a>>] {
        self.inner.as_slice()
    }

    pub fn first(&self) -> &'a MapArrayToplevelEntry<'a> {
        self.inner.first().get()
    }

    pub fn last(&self) -> &MapArrayToplevelEntry<'a> {
        self.inner.last().get()
    }

    pub fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = &MapArrayToplevelEntry<'a>> + DoubleEndedIterator {
        self.inner.iter().map(|e| e.get())
    }
}

#[derive(Debug, PartialEq)]
pub struct MapArrayToplevelEntry<'a, S = Complete> {
    pub node: CyclicCell<'a, MapTable<'a, S>, S>,
    /// The definition of this array of tables entry.
    /// ```toml
    /// [[a.b]] # this here
    /// ```
    pub definition: &'a ArrayEntry<'a>,
    pub parent: ParentToplevelArray<'a, S>,
    pub parent_entry: ParentEntry<'a, S>,
    /// A list of tables that extend the this array of tables entry.
    /// ```toml
    /// [[a.b]] # another entry
    ///
    /// [[a.b]] # the entry that is extended
    ///
    /// [a.b.c] # the extension
    /// ```
    pub extensions: Vec<ParentTableEntry<'a, S>>,
    pub idx: u32,
}

impl<'a, S> MapArrayToplevelEntry<'a, S> {
    pub fn new(
        node: CyclicCell<'a, MapTable<'a, S>, S>,
        repr: &'a ArrayEntry<'a>,
        parent_entry: ParentEntry<'a, S>,
        parent: ParentToplevelArray<'a, S>,
        idx: u32,
    ) -> Self {
        Self {
            node,
            definition: repr,
            parent_entry,
            parent,
            extensions: Vec::new(),
            idx,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum MapArrayToplevelEntryRepr<'a, S = Complete> {
    Definition(&'a ArrayEntry<'a>, ParentToplevelArray<'a, S>),
    Extension(ParentTable<'a, S>),
}

#[derive(Debug, PartialEq)]
pub struct MapArrayInline<'a, S = Complete> {
    pub parent: ParentEntry<'a, S>,
    pub repr: &'a InlineArray<'a>,
    inner: &'a [MapArrayInlineEntry<'a, S>],
}

impl<'a, S> MapArrayInline<'a, S> {
    pub fn new(
        parent: ParentEntry<'a, S>,
        repr: &'a InlineArray<'a>,
        inner: &'a [MapArrayInlineEntry<'a, S>],
    ) -> Self {
        Self {
            parent,
            repr,
            inner,
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }

    pub fn get(&self, idx: usize) -> Option<&'a MapArrayInlineEntry<'a, S>> {
        self.inner.get(idx)
    }

    pub fn as_slice(&self) -> &'a [MapArrayInlineEntry<'a, S>] {
        self.inner
    }

    pub fn iter(&self) -> impl Iterator<Item = &'a MapArrayInlineEntry<'a, S>> {
        self.inner.iter()
    }
}

impl<'a, I: std::slice::SliceIndex<[MapArrayInlineEntry<'a>]>> std::ops::Index<I>
    for MapArrayInline<'a>
{
    type Output = I::Output;

    #[inline]
    fn index(&self, index: I) -> &Self::Output {
        std::ops::Index::index(self.inner, index)
    }
}

#[derive(Debug, PartialEq)]
pub struct MapArrayInlineEntry<'a, S = Complete> {
    pub node: MapNode<'a, S>,
    pub repr: &'a InlineArrayValue<'a>,
    pub parent: ParentInlineArray<'a, S>,
    pub idx: u32,
}

impl<'a, S> MapArrayInlineEntry<'a, S> {
    pub fn new(
        node: MapNode<'a, S>,
        repr: &'a InlineArrayValue<'a>,
        parent: ParentInlineArray<'a, S>,
        idx: u32,
    ) -> Self {
        Self {
            node,
            repr,
            parent,
            idx,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum MapNode<'a, S = Complete> {
    Table(CyclicCell<'a, MapTable<'a, S>, S>),
    Array(MapArray<'a, S>),
    Scalar(Scalar<'a>),
}

impl<'a> MapNode<'a> {
    pub fn as_table(&self) -> Option<&MapTable<'a>> {
        match self {
            MapNode::Table(table) => Some(table.get()),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&MapArray<'a>> {
        match self {
            MapNode::Array(array) => Some(array),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&StringVal<'a>> {
        match self {
            MapNode::Scalar(Scalar::String(val)) => Some(val),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<&IntVal> {
        match self {
            MapNode::Scalar(Scalar::Int(val)) => Some(val),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<&FloatVal> {
        match self {
            MapNode::Scalar(Scalar::Float(val)) => Some(val),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<&BoolVal> {
        match self {
            MapNode::Scalar(Scalar::Bool(val)) => Some(val),
            _ => None,
        }
    }

    pub fn as_date_time(&self) -> Option<&DateTimeVal> {
        match self {
            MapNode::Scalar(Scalar::DateTime(val)) => Some(val),
            _ => None,
        }
    }
}

// TODO: Add ParentEntry to scalars
#[derive(Debug, PartialEq)]
pub enum Scalar<'a> {
    String(&'a StringVal<'a>),
    Int(&'a IntVal),
    Float(&'a FloatVal),
    Bool(&'a BoolVal),
    DateTime(&'a DateTimeVal),
    Invalid(&'a Span),
}

impl Scalar<'_> {
    #[inline]
    pub fn span(&self) -> Span {
        match self {
            Scalar::String(s) => s.lit_span,
            Scalar::Int(i) => i.lit_span,
            Scalar::Float(f) => f.lit_span,
            Scalar::Bool(b) => b.lit_span,
            Scalar::DateTime(d) => d.lit_span,
            Scalar::Invalid(span) => **span,
        }
    }
}

pub fn map<'a>(ctx: &mut impl TomlCtx, bump: &'a Bump, ast: &Ast<'a>) -> &'a MapTable<'a> {
    let (errors, map) = construct::map(bump, ast);
    set_mapped_table(map);
    for e in errors {
        ctx.error(convert_error(e));
    }
    map
}

fn set_mapped_table<'a>(map: &'a MapTable<'a>) {
    for (repr, idx) in map.reprs.iter().zip(0..) {
        match repr {
            MapTableRepr::Root(_) => (),
            MapTableRepr::Table(table, _) => {
                let parent_table = ParentTable::new(Cyclic::new(map), ReprIdx(idx));
                // SAFETY: The map isn't shared between threads.
                unsafe { table.mapped.set(parent_table) }
            }
            MapTableRepr::InlineTable(inline_table, _) => {
                let parent_table = ParentTable::new(Cyclic::new(map), ReprIdx(idx));
                // SAFETY: The map isn't shared between threads.
                unsafe { inline_table.mapped.set(parent_table) }
            }
            MapTableRepr::ArrayEntry(..) => (),
            MapTableRepr::ToplevelAssignment(..) => (),
            MapTableRepr::InlineTableAssignment(..) => (),
        }
    }

    for (_, entry) in map.inner.iter() {
        for (repr, idx) in entry.reprs.iter().zip(0..) {
            let parent_entry = ParentTableEntry::new(Cyclic::new(entry.get()), ReprIdx(idx));
            // SAFETY: The map isn't shared between threads.
            unsafe { repr.key.repr_ident().mapped.set(parent_entry) };
        }

        set_mapped_node(&entry.node);
    }
}

fn set_mapped_toplevel_array<'a>(array: &'a MapArrayToplevel<'a>) {
    for entry in array.iter() {
        let parent_entry = ParentToplevelArrayEntry::new(Cyclic::new(entry));
        // SAFETY: The map isn't shared between threads.
        unsafe { entry.definition.mapped.set(parent_entry) };

        set_mapped_table(entry.node.get());
    }
}

fn set_mapped_inline_array<'a>(array: &'a MapArrayInline<'a>) {
    let parent_array = ParentInlineArray::new(Cyclic::new(array));
    // SAFETY: The map isn't shared between threads.
    unsafe { array.repr.mapped.set(parent_array) };

    for entry in array.iter() {
        let parent_entry = ParentInlineArrayEntry::new(Cyclic::new(entry));
        // SAFETY: The map isn't shared between threads.
        unsafe { entry.repr.mapped.set(parent_entry) };

        set_mapped_node(&entry.node);
    }
}

fn set_mapped_node<'a>(node: &'a MapNode<'a>) {
    match node {
        MapNode::Table(map) => set_mapped_table(map.get()),
        MapNode::Array(MapArray::Toplevel(array)) => set_mapped_toplevel_array(array.get()),
        MapNode::Array(MapArray::Inline(array)) => set_mapped_inline_array(array),
        MapNode::Scalar(_) => (),
    }
}

fn convert_error(error: MapError<Complete>) -> Error {
    let parents = [error.orig_parent, error.new_parent]
        .into_iter()
        .filter_map(|p| p.repr().parent_entry());
    let lines = context_lines(parents);
    let path = joined_path(error.new_parent, error.new_ident);
    let orig = error.orig_span;
    let new = error.new_ident.lit_span();

    match error.kind {
        MapErrorKind::DuplicateKey => Error::DuplicateKey {
            lines,
            path,
            orig,
            new,
        },
        MapErrorKind::CannotExtendTableWithDottedKey => Error::CannotExtendTableWithDottedKey {
            lines,
            path,
            orig,
            new,
        },
        MapErrorKind::CannotExtendInlineTable => Error::CannotExtendInlineTable {
            lines,
            path,
            orig,
            new,
        },
        MapErrorKind::CannotExtendArrayWithDottedKey => Error::CannotExtendArrayWithDottedKey {
            lines,
            path,
            orig,
            new,
        },
        MapErrorKind::CannotExtendInlineArray => Error::CannotExtendInlineArray {
            lines,
            path,
            orig,
            new,
        },
        MapErrorKind::CannotExtendInlineArrayAsTable => Error::CannotExtendInlineArrayAsTable {
            lines,
            path,
            orig,
            new,
        },
    }
}
