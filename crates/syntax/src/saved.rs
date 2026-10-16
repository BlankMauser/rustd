//! A parse as it is saved: the tree as the events that build it and the text its tokens
//! cover, in order. Loading it builds the green tree again without lexing or parsing.

use std::marker::PhantomData;

use rowan::{GreenNode, GreenNodeBuilder, NodeOrToken, WalkEvent};
use serde::{Deserialize, Serialize};

use crate::{Parse, SyntaxError, TextRange, TextSize};

#[derive(Serialize, Deserialize)]
enum Event {
    Enter(u16),
    /// A token of this kind over the next so many bytes of the text.
    Token(u16, u32),
    Leave,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    events: Vec<Event>,
    text: String,
    errors: Vec<(String, u32, u32)>,
}

impl Saved {
    fn of(green: &GreenNode, errors: &[SyntaxError]) -> Saved {
        let (mut events, mut text) = (Vec::new(), String::new());
        let root = rowan::SyntaxNode::<crate::RustLanguage>::new_root(green.clone());
        for event in root.preorder_with_tokens() {
            match event {
                WalkEvent::Enter(NodeOrToken::Node(node)) => {
                    events.push(Event::Enter(node.green().kind().0))
                }
                WalkEvent::Enter(NodeOrToken::Token(token)) => {
                    events.push(Event::Token(token.green().kind().0, token.text().len() as u32));
                    text.push_str(token.text());
                }
                WalkEvent::Leave(NodeOrToken::Node(_)) => events.push(Event::Leave),
                WalkEvent::Leave(NodeOrToken::Token(_)) => {}
            }
        }
        let errors = errors
            .iter()
            .map(|it| (it.to_string(), it.range().start().into(), it.range().end().into()))
            .collect();
        Saved { events, text, errors }
    }

    fn green(&self) -> GreenNode {
        let (mut builder, mut at) = (GreenNodeBuilder::new(), 0);
        for event in &self.events {
            match *event {
                Event::Enter(kind) => builder.start_node(rowan::SyntaxKind(kind)),
                Event::Token(kind, len) => {
                    let end = at + len as usize;
                    builder.token(rowan::SyntaxKind(kind), &self.text[at..end]);
                    at = end;
                }
                Event::Leave => builder.finish_node(),
            }
        }
        builder.finish()
    }
}

impl<T> Serialize for Parse<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let green = self.green.as_ref().ok_or_else(|| serde::ser::Error::custom("no tree"))?;
        Saved::of(green, self.errors.as_deref().unwrap_or_default()).serialize(serializer)
    }
}

impl<'de, T> Deserialize<'de> for Parse<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let saved = Saved::deserialize(deserializer)?;
        let errors: Vec<SyntaxError> = saved
            .errors
            .iter()
            .map(|(message, start, end)| {
                let range = TextRange::new(TextSize::new(*start), TextSize::new(*end));
                SyntaxError::new(message.clone(), range)
            })
            .collect();
        Ok(Parse {
            green: Some(saved.green()),
            errors: (!errors.is_empty()).then(|| errors.into()),
            _ty: PhantomData,
        })
    }
}
