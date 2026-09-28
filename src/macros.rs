//! A small `macro_rules!` expander for item macros that declare types, such as newtype
//! generators. Anything it cannot match exactly is left unexpanded.
use proc_macro2::{Delimiter, Group, Ident, Spacing, TokenStream, TokenTree};
use std::collections::HashMap;

pub(crate) struct Rules {
    arms: Vec<(Vec<TokenTree>, Vec<TokenTree>)>,
}
#[derive(Clone)]
enum Fragment {
    One(Vec<TokenTree>),
    Many(Vec<Fragment>),
}
type Bindings = HashMap<String, Fragment>;

impl Rules {
    /// Parse `(pattern) => { body };` arms.
    pub fn parse(tokens: TokenStream) -> Option<Self> {
        let tokens: Vec<TokenTree> = tokens.into_iter().collect();
        let mut arms = Vec::new();
        let mut rest = tokens.as_slice();
        while let [
            TokenTree::Group(pattern),
            TokenTree::Punct(eq),
            TokenTree::Punct(gt),
            TokenTree::Group(body),
            tail @ ..,
        ] = rest
        {
            if eq.as_char() != '=' || gt.as_char() != '>' {
                return None;
            }
            arms.push((
                pattern.stream().into_iter().collect(),
                body.stream().into_iter().collect(),
            ));
            rest = match tail {
                [TokenTree::Punct(p), tail @ ..] if p.as_char() == ';' => tail,
                tail => tail,
            };
        }
        (rest.is_empty() && !arms.is_empty()).then_some(Self { arms })
    }
    /// Expand an invocation with the first arm that matches all of its input.
    pub fn expand(&self, input: TokenStream) -> Option<TokenStream> {
        let input: Vec<TokenTree> = input.into_iter().collect();
        self.arms.iter().find_map(|(pattern, body)| {
            let mut bindings = Bindings::new();
            (matches(pattern, &input, None, &mut bindings)? == input.len())
                .then(|| transcribe(body, &bindings))
                .flatten()
                .map(|t| t.into_iter().collect())
        })
    }
}
fn same(a: &TokenTree, b: &TokenTree) -> bool {
    match (a, b) {
        (TokenTree::Ident(a), TokenTree::Ident(b)) => a == b,
        (TokenTree::Punct(a), TokenTree::Punct(b)) => a.as_char() == b.as_char(),
        (TokenTree::Literal(a), TokenTree::Literal(b)) => a.to_string() == b.to_string(),
        (TokenTree::Group(a), TokenTree::Group(b)) => {
            a.delimiter() == b.delimiter() && a.stream().to_string() == b.stream().to_string()
        }
        _ => false,
    }
}
fn dollar(t: &TokenTree) -> bool {
    matches!(t, TokenTree::Punct(p) if p.as_char() == '$')
}
/// A repetition `$( ... ) sep? op`: (inner pattern, separator, operator, tokens used).
fn repetition(pattern: &[TokenTree]) -> Option<(Vec<TokenTree>, Option<TokenTree>, char, usize)> {
    let [d, TokenTree::Group(g), rest @ ..] = pattern else {
        return None;
    };
    if !dollar(d) || g.delimiter() != Delimiter::Parenthesis {
        return None;
    }
    let op = |t: &TokenTree| match t {
        TokenTree::Punct(p) if ['*', '+', '?'].contains(&p.as_char()) => Some(p.as_char()),
        _ => None,
    };
    let inner = g.stream().into_iter().collect();
    match rest {
        [o, ..] if op(o).is_some() => Some((inner, None, op(o)?, 3)),
        [sep, o, ..] => Some((inner, Some(sep.clone()), op(o)?, 4)),
        _ => None,
    }
}
/// Match a pattern against a prefix of `input`; returns how many tokens it consumed.
fn matches(
    pattern: &[TokenTree],
    input: &[TokenTree],
    follow: Option<&TokenTree>,
    out: &mut Bindings,
) -> Option<usize> {
    let (mut p, mut i) = (0, 0);
    while p < pattern.len() {
        if let Some((inner, sep, op, used)) = repetition(&pattern[p..]) {
            let after = pattern.get(p + used).or(follow);
            let mut rounds: Vec<Bindings> = Vec::new();
            loop {
                let mut b = Bindings::new();
                let Some(n) =
                    matches(&inner, &input[i..], sep.as_ref().or(after), &mut b).filter(|&n| n > 0)
                else {
                    break;
                };
                i += n;
                rounds.push(b);
                if op == '?' {
                    break;
                }
                match (&sep, input.get(i)) {
                    (Some(s), Some(t)) if same(s, t) => {
                        let mut probe = Bindings::new();
                        if matches(&inner, &input[i + 1..], sep.as_ref().or(after), &mut probe)
                            .is_some_and(|n| n > 0)
                        {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    (Some(_), _) => break,
                    (None, _) => {}
                }
            }
            if op == '+' && rounds.is_empty() {
                return None;
            }
            for name in rounds
                .iter()
                .flat_map(|b| b.keys())
                .cloned()
                .collect::<Vec<_>>()
            {
                let each = rounds
                    .iter()
                    .map(|b| b.get(&name).cloned())
                    .collect::<Option<Vec<_>>>()?;
                out.insert(name, Fragment::Many(each));
            }
            p += used;
            continue;
        }
        if let [
            d,
            TokenTree::Ident(name),
            TokenTree::Punct(colon),
            TokenTree::Ident(kind),
            ..,
        ] = &pattern[p..]
            && dollar(d)
            && colon.as_char() == ':'
        {
            let next = pattern.get(p + 4).or(follow);
            let n = fragment(&kind.to_string(), &input[i..], next)?;
            out.insert(name.to_string(), Fragment::One(input[i..i + n].to_vec()));
            i += n;
            p += 4;
            continue;
        }
        let t = input.get(i)?;
        match (&pattern[p], t) {
            (TokenTree::Group(a), TokenTree::Group(b)) if a.delimiter() == b.delimiter() => {
                let inner: Vec<TokenTree> = b.stream().into_iter().collect();
                let pat: Vec<TokenTree> = a.stream().into_iter().collect();
                if matches(&pat, &inner, None, out)? != inner.len() {
                    return None;
                }
            }
            (a, b) if !matches!(a, TokenTree::Group(_)) && same(a, b) => {}
            _ => return None,
        }
        p += 1;
        i += 1;
    }
    Some(i)
}
/// How many tokens a fragment specifier consumes, stopping before `next` when it is a literal token.
fn fragment(kind: &str, input: &[TokenTree], next: Option<&TokenTree>) -> Option<usize> {
    match kind {
        "ident" => matches!(input.first()?, TokenTree::Ident(_)).then_some(1),
        "tt" => input.first().map(|_| 1),
        "literal" => match input {
            [TokenTree::Literal(_), ..] => Some(1),
            [TokenTree::Punct(p), TokenTree::Literal(_), ..] if p.as_char() == '-' => Some(2),
            [TokenTree::Ident(i), ..] if i == "true" || i == "false" => Some(1),
            _ => None,
        },
        "lifetime" => match input {
            [TokenTree::Punct(p), TokenTree::Ident(_), ..] if p.as_char() == '\'' => Some(2),
            _ => None,
        },
        "block" => {
            matches!(input.first()?, TokenTree::Group(g) if g.delimiter() == Delimiter::Brace)
                .then_some(1)
        }
        "vis" => match input {
            [TokenTree::Ident(i), TokenTree::Group(g), ..]
                if i == "pub" && g.delimiter() == Delimiter::Parenthesis =>
            {
                Some(2)
            }
            [TokenTree::Ident(i), ..] if i == "pub" => Some(1),
            _ => Some(0),
        },
        "ty" | "path" | "expr" | "pat" | "pat_param" | "meta" | "item" | "stmt" => {
            // Take tokens up to the next literal pattern token, outside angle brackets.
            let stop = match next {
                Some(t) if !dollar(t) => Some(t),
                Some(_) => return None,
                None => None,
            };
            let mut depth = 0i32;
            let mut n = 0;
            while let Some(t) = input.get(n) {
                if depth == 0 && stop.is_some_and(|s| same(s, t)) {
                    break;
                }
                if let TokenTree::Punct(p) = t {
                    let arrow = p.as_char() == '>'
                        && n > 0
                        && matches!(&input[n - 1], TokenTree::Punct(q) if (q.as_char() == '-' || q.as_char() == '=') && q.spacing() == Spacing::Joint);
                    match p.as_char() {
                        '<' if kind == "ty" || kind == "path" => depth += 1,
                        '>' if !arrow && (kind == "ty" || kind == "path") => depth -= 1,
                        _ => {}
                    }
                }
                n += 1;
            }
            let tokens: TokenStream = input[..n].iter().cloned().collect();
            let ok = match kind {
                "ty" => syn::parse2::<syn::Type>(tokens).is_ok(),
                "path" => syn::parse2::<syn::Path>(tokens).is_ok(),
                "meta" => syn::parse2::<syn::Meta>(tokens).is_ok(),
                "item" => syn::parse2::<syn::Item>(tokens).is_ok(),
                _ => n > 0,
            };
            ok.then_some(n)
        }
        _ => None,
    }
}
fn transcribe(body: &[TokenTree], bindings: &Bindings) -> Option<Vec<TokenTree>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        if let Some((inner, sep, _, used)) = repetition(&body[i..]) {
            let names: Vec<&String> = bindings
                .iter()
                .filter(|(n, f)| matches!(f, Fragment::Many(_)) && mentions(&inner, n))
                .map(|(n, _)| n)
                .collect();
            let count = names
                .iter()
                .map(|n| match &bindings[*n] {
                    Fragment::Many(v) => v.len(),
                    Fragment::One(_) => 0,
                })
                .max()?;
            for round in 0..count {
                let mut b = bindings.clone();
                for n in &names {
                    if let Fragment::Many(v) = &bindings[*n] {
                        b.insert((*n).clone(), v.get(round)?.clone());
                    }
                }
                if round > 0 {
                    out.extend(sep.clone());
                }
                out.extend(transcribe(&inner, &b)?);
            }
            i += used;
            continue;
        }
        match (&body[i], body.get(i + 1)) {
            (d, Some(TokenTree::Ident(name))) if dollar(d) => {
                if name == "crate" {
                    out.push(TokenTree::Ident(Ident::new("crate", name.span())));
                } else {
                    match bindings.get(&name.to_string())? {
                        Fragment::One(tokens) if tokens.len() == 1 => {
                            out.extend(tokens.iter().cloned())
                        }
                        // Macro fragments keep their grouping, like rustc's invisible delimiters.
                        Fragment::One(tokens) => out.push(TokenTree::Group(Group::new(
                            Delimiter::None,
                            tokens.iter().cloned().collect(),
                        ))),
                        Fragment::Many(_) => return None,
                    }
                }
                i += 2;
            }
            (TokenTree::Group(g), _) => {
                let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                let mut group = Group::new(
                    g.delimiter(),
                    transcribe(&inner, bindings)?.into_iter().collect(),
                );
                group.set_span(g.span());
                out.push(TokenTree::Group(group));
                i += 1;
            }
            (t, _) => {
                out.push(t.clone());
                i += 1;
            }
        }
    }
    Some(out)
}
fn mentions(tokens: &[TokenTree], name: &str) -> bool {
    tokens
        .windows(2)
        .any(|w| dollar(&w[0]) && matches!(&w[1], TokenTree::Ident(i) if i == name))
        || tokens.iter().any(|t| match t {
            TokenTree::Group(g) => mentions(&g.stream().into_iter().collect::<Vec<_>>(), name),
            _ => false,
        })
}
