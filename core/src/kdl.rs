use std::{fmt, path::PathBuf};

pub use ::kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use miette::SourceSpan;

#[macro_export]
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

#[macro_export]
macro_rules! props {
    ($node:expr, $($kind:ident)? $name:literal $(= $default:expr)? $(,)?) => {
        {
            let node = $node;
            $crate::kdl::properties(node, &[$name]).and_then(|()| {
                $crate::kdl::props!(@get node, $($kind)? $name $(= $default)?)
            })
        }
    };
    ($node:expr, $($( $kind:ident)? $name:literal $(= $default:expr)?),+ $(,)?) => {
        {
            let node = $node;
            $crate::kdl::properties(node, &[$($name),+]).and_then(|()| {
                Ok::<_, $crate::kdl::Issue>(($($crate::kdl::props!(@get node, $($kind)? $name $(= $default)?)?),+,))
            })
        }
    };
    (@get $node:ident, $name:literal) => {
        $crate::kdl::property($node, $name)
    };
    (@get $node:ident, optional $name:literal) => {
        $crate::kdl::optional_property($node, $name)
    };
    (@get $node:ident, $name:literal = $default:expr) => {
        $crate::kdl::optional_property($node, $name).map(|value| value.unwrap_or_else(|| $default))
    };
}

#[macro_export]
macro_rules! nodes {
    ($document:expr, $($( $kind:ident)? $name:literal $(= $default:expr)?),+ $(,)?) => {
        {
            let document = $document;
            $crate::kdl::validate_nodes(document, &[$($name),+]).and_then(|()| {
                Ok::<_, $crate::kdl::Issue>(($($crate::kdl::nodes!(@get document, $($kind)? $name $(= $default)?)?),+,))
            })
        }
    };
    (@get $document:ident, $name:literal) => {
        $crate::kdl::optional_node($document, $name)
            .and_then(|value| value.ok_or_else(|| $crate::kdl::issue($document, concat!("missing ", $name, " node"))))
    };
    (@get $document:ident, optional $name:literal) => {
        $crate::kdl::optional_node($document, $name)
    };
    (@get $document:ident, many $name:literal) => {
        Ok($crate::kdl::all($document, $name))
    };
    (@get $document:ident, $name:literal = $default:expr) => {
        $crate::kdl::optional_node($document, $name)
            .map(|value| value.unwrap_or_else(|| $default))
    };
}

pub use crate::{nodes, one_of, props};

pub trait Span {
    fn span(&self) -> SourceSpan;
}

#[derive(Debug)]
pub struct Issue {
    pub message: String,
    pub span: SourceSpan,
}

#[derive(Debug, Clone, Copy)]
pub struct Spanned<T> {
    pub value: T,
    pub span: SourceSpan,
}

impl fmt::Display for Issue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Issue {}

pub trait FromKdl<'a>: Sized {
    fn from_kdl(entry: &'a KdlEntry, field: &str) -> Result<Self, Issue>;
}

pub trait FromKdlNode<'a>: Sized {
    fn from_kdl_node(node: &'a KdlNode) -> Result<Self, Issue>;
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

impl<T> Span for Spanned<T> {
    fn span(&self) -> SourceSpan {
        self.span
    }
}

impl<'a, T: FromKdl<'a>> FromKdl<'a> for Spanned<T> {
    fn from_kdl(entry: &'a KdlEntry, field: &str) -> Result<Self, Issue> {
        Ok(Self {
            value: T::from_kdl(entry, field)?,
            span: entry.span(),
        })
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

impl FromKdl<'_> for PathBuf {
    fn from_kdl(entry: &KdlEntry, field: &str) -> Result<Self, Issue> {
        <&str>::from_kdl(entry, field).map(Self::from)
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

impl<'a, T: FromKdl<'a>> FromKdlNode<'a> for T {
    fn from_kdl_node(node: &'a KdlNode) -> Result<Self, Issue> {
        properties(node, &[])?;
        argument(node, 0)
    }
}

impl<'a> FromKdlNode<'a> for &'a KdlNode {
    fn from_kdl_node(node: &'a KdlNode) -> Result<Self, Issue> {
        Ok(node)
    }
}

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

pub fn optional_node<'a, T: FromKdlNode<'a>>(
    document: &'a KdlDocument,
    name: &str,
) -> Result<Option<T>, Issue> {
    let mut nodes = document
        .nodes()
        .iter()
        .filter(|node| node.name().value() == name);
    let node = nodes.next();
    if let Some(duplicate) = nodes.next() {
        Err(issue(duplicate, format!("duplicate {name} node")))
    } else {
        node.map(T::from_kdl_node).transpose()
    }
}

pub fn all<'a>(
    document: &'a KdlDocument,
    name: &'a str,
) -> impl Iterator<Item = &'a KdlNode> + Clone {
    document
        .nodes()
        .iter()
        .filter(move |node| node.name().value() == name)
}

pub fn validate_nodes(document: &KdlDocument, allowed: &[&str]) -> Result<(), Issue> {
    if let Some(node) = document
        .nodes()
        .iter()
        .find(|node| !allowed.contains(&node.name().value()))
    {
        Err(issue(
            node,
            format!("unexpected node {:?}", node.name().value()),
        ))
    } else {
        Ok(())
    }
}

pub fn issue(value: &impl Span, message: impl fmt::Display) -> Issue {
    Issue {
        message: message.to_string(),
        span: value.span(),
    }
}
