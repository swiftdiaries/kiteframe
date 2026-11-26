use std::collections::BTreeMap;

use serde_json::Value;

use super::{OneOfPrimitive, PolicyEffect, Primitive};

pub(super) enum Argument<'a> {
    Missing,
    Comparable(&'a Value),
    Uncomparable,
}

pub(super) fn argument_at<'a>(arguments: &'a Value, pointer: &str) -> Argument<'a> {
    match arguments.pointer(pointer) {
        None | Some(Value::Null) => Argument::Missing,
        Some(value) if value.is_string() || value.is_boolean() || value.as_i64().is_some() => {
            Argument::Comparable(value)
        }
        Some(_) => Argument::Uncomparable,
    }
}
pub(super) fn input_matches(
    effect: PolicyEffect,
    input: &BTreeMap<String, OneOfPrimitive>,
    arguments: &Value,
) -> bool {
    input.iter().all(
        |(pointer, allowed)| match (effect, argument_at(arguments, pointer)) {
            (PolicyEffect::Forbid, Argument::Missing | Argument::Uncomparable) => true,
            (PolicyEffect::Permit, Argument::Missing) => allowed.0.contains(&Primitive::Null),
            (PolicyEffect::Permit, Argument::Uncomparable) => false,
            (_, Argument::Comparable(value)) => {
                allowed.0.iter().any(|primitive| equals(primitive, value))
            }
        },
    )
}

fn equals(primitive: &Primitive, value: &Value) -> bool {
    match primitive {
        Primitive::String(expected) => value.as_str() == Some(expected),
        Primitive::Integer(expected) => value.as_i64() == Some(*expected),
        Primitive::Boolean(expected) => value.as_bool() == Some(*expected),
        Primitive::Number(_) | Primitive::Null => false,
    }
}
