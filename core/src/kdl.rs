use std::fmt;

pub use ::kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use miette::SourceSpan;

macro_rules! one_of {
    ($node:expr, $value:expr, $expected:literal, {$($($name:literal)|+ => $result:expr),+ $(,)?}) => {
        match $value {
            $($($name)|+ => Ok($result),)+
            value => Err($crate::kdl::issue(
                $node,
                format!(
                    "expected {} to be one of {:?}, got {value:?}",
                    $expected,
                    &[$($($name),+),+],
                ),
            )),
        }
    };
}

macro_rules! required_properties {
    ($node:expr, $($name:literal),+ $(,)?) => {{
        $crate::kdl::properties($node, &[$($name),+])?;
        Ok::<_, $crate::kdl::Issue>(($($crate::kdl::property($node, $name)?),+,))
    }};
}

pub(crate) use {one_of, required_properties};

pub trait Span {
    fn span(&self) -> SourceSpan;
}

#[derive(Debug)]
pub struct Issue {
    pub message: String,
    pub span: SourceSpan,
}

pub trait FromKdl<'a>: Sized {
    fn from_kdl(entry: &'a KdlEntry, field: &str) -> Result<Self, Issue>;
}

impl Span for KdlEntry {
    fn span(&self) -> SourceSpan {
        self.span()
    }
}

impl Span for KdlNode {
    fn span(&self) -> SourceSpan {
        self.span()
    }
}

impl Span for KdlDocument {
    fn span(&self) -> SourceSpan {
        self.span()
    }
}

impl<'a> FromKdl<'a> for &'a str {
    fn from_kdl(entry: &'a KdlEntry, field: &str) -> Result<Self, Issue> {
        entry
            .value()
            .as_string()
            .ok_or_else(|| issue(entry, format!("{field} must be a string")))
    }
}

impl FromKdl<'_> for bool {
    fn from_kdl(entry: &KdlEntry, field: &str) -> Result<Self, Issue> {
        entry
            .value()
            .as_bool()
            .ok_or_else(|| issue(entry, format!("{field} must be a boolean")))
    }
}

impl FromKdl<'_> for f32 {
    fn from_kdl(entry: &KdlEntry, field: &str) -> Result<Self, Issue> {
        match entry.value() {
            KdlValue::Float(value) => Ok(*value as f32),
            KdlValue::Integer(value) => Ok(*value as f32),
            _ => Err(issue(entry, format!("{field} must be a number"))),
        }
    }
}

macro_rules! integer {
    ($($type:ty),+ $(,)?) => {
        $(
            impl FromKdl<'_> for $type {
                fn from_kdl(entry: &KdlEntry, field: &str) -> Result<Self, Issue> {
                    let value = entry
                        .value()
                        .as_integer()
                        .ok_or_else(|| issue(entry, format!("{field} must be an integer")))?;
                    value
                        .try_into()
                        .map_err(|_| issue(entry, format!("{field} is out of range")))
                }
            }
        )+
    };
}

integer!(
    i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize
);

pub fn argument<'a, T: FromKdl<'a>>(node: &'a KdlNode, index: usize) -> Result<T, Issue> {
    let entry = node.entry(index).ok_or_else(|| {
        issue(
            node,
            format!("{} requires argument {index}", node.name().value()),
        )
    })?;
    T::from_kdl(entry, "argument")
}

pub fn property<'a, T: FromKdl<'a>>(node: &'a KdlNode, name: &str) -> Result<T, Issue> {
    let entry = node.entry(name).ok_or_else(|| {
        issue(
            node,
            format!("{} requires property {name:?}", node.name().value()),
        )
    })?;
    T::from_kdl(entry, name)
}

pub fn optional_property<'a, T: FromKdl<'a>>(
    node: &'a KdlNode,
    name: &str,
) -> Result<Option<T>, Issue> {
    node.entry(name)
        .map(|entry| T::from_kdl(entry, name))
        .transpose()
}

pub fn properties(node: &KdlNode, allowed: &[&str]) -> Result<(), Issue> {
    for (index, entry) in node.entries().iter().enumerate() {
        if let Some(name) = entry.name().map(|name| name.value()) {
            if !allowed.contains(&name) {
                return Err(issue(
                    entry,
                    format!("{} has unexpected property {name:?}", node.name().value()),
                ));
            }
            if node.entries()[..index].iter().any(|earlier| {
                earlier
                    .name()
                    .is_some_and(|earlier| earlier.value() == name)
            }) {
                return Err(issue(
                    entry,
                    format!("{} has duplicate property {name:?}", node.name().value()),
                ));
            }
        }
    }
    Ok(())
}

pub fn children(node: &KdlNode) -> &[KdlNode] {
    node.children().map(KdlDocument::nodes).unwrap_or_default()
}

pub fn arguments(node: &KdlNode) -> impl Iterator<Item = &KdlValue> {
    node.entries()
        .iter()
        .filter(|entry| entry.name().is_none())
        .map(|entry| entry.value())
}

pub fn single<'a>(document: &'a KdlDocument, name: &str) -> Result<Option<&'a KdlNode>, Issue> {
    let mut nodes = document
        .nodes()
        .iter()
        .filter(|node| node.name().value() == name);
    let node = nodes.next();
    if let Some(duplicate) = nodes.next() {
        Err(issue(duplicate, format!("duplicate {name} node")))
    } else {
        Ok(node)
    }
}

pub fn issue(value: &impl Span, message: impl fmt::Display) -> Issue {
    Issue {
        message: message.to_string(),
        span: value.span(),
    }
}
