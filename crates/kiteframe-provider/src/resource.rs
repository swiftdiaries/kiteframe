#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedSelector {
    segments: Vec<String>,
    separators: Vec<char>,
}

impl ParsedSelector {
    fn parse(value: &str) -> Result<Self, String> {
        if value.contains("${context.") {
            return Err("resource selector contains an unresolved context placeholder".to_owned());
        }

        let mut segments = Vec::new();
        let mut separators = Vec::new();
        let mut current = String::new();
        for character in value.chars() {
            if matches!(character, '/' | ':') {
                if current.is_empty() {
                    return Err("resource selector must contain non-empty segments".to_owned());
                }
                segments.push(std::mem::take(&mut current));
                separators.push(character);
            } else {
                current.push(character);
            }
        }
        if current.is_empty() {
            return Err("resource selector must contain non-empty segments".to_owned());
        }
        segments.push(current);

        if segments
            .iter()
            .any(|segment| segment.contains('*') && segment != "*")
        {
            return Err("resource selector wildcard must occupy a complete segment".to_owned());
        }
        Ok(Self {
            segments,
            separators,
        })
    }

    fn render(&self) -> String {
        let mut rendered = self.segments[0].clone();
        for (separator, segment) in self.separators.iter().zip(self.segments.iter().skip(1)) {
            rendered.push(*separator);
            rendered.push_str(segment);
        }
        rendered
    }
}

pub fn validate_concrete_resource_selector(value: &str) -> Result<(), String> {
    let parsed = ParsedSelector::parse(value)?;
    if parsed.segments.iter().any(|segment| segment == "*") {
        return Err("invocation resource selector must not contain wildcards".to_owned());
    }
    Ok(())
}

pub fn resource_selector_is_subset(candidate: &str, allowed: &str) -> bool {
    let Ok(candidate) = ParsedSelector::parse(candidate) else {
        return false;
    };
    let Ok(allowed) = ParsedSelector::parse(allowed) else {
        return false;
    };
    candidate.separators == allowed.separators
        && candidate.segments.len() == allowed.segments.len()
        && candidate
            .segments
            .iter()
            .zip(&allowed.segments)
            .all(|(candidate, allowed)| candidate == allowed || allowed == "*")
}

pub fn intersect_resource_selectors(
    left: &str,
    right: &str,
) -> Result<Option<String>, String> {
    let left = ParsedSelector::parse(left)?;
    let right = ParsedSelector::parse(right)?;
    if left.separators != right.separators || left.segments.len() != right.segments.len() {
        return Ok(None);
    }

    let mut intersection = Vec::with_capacity(left.segments.len());
    for (left, right) in left.segments.iter().zip(&right.segments) {
        match (left.as_str(), right.as_str()) {
            (left, right) if left == right => intersection.push(left.to_owned()),
            ("*", right) => intersection.push(right.to_owned()),
            (left, "*") => intersection.push(left.to_owned()),
            _ => return Ok(None),
        }
    }
    Ok(Some(
        ParsedSelector {
            segments: intersection,
            separators: left.separators,
        }
        .render(),
    ))
}
